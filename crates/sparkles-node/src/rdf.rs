//! Standalone RDF streams. Workers own Rust values only; bounded channels apply
//! backpressure in both directions, including while JavaScript is not polling.
use super::*;
use napi::bindgen_prelude::{Buffer, PromiseRaw};
use oxrdfio::{RdfParser, RdfSerializer};
use std::io::{Read, Write};
use tokio::sync::mpsc;

const CHUNK: usize = 64 << 10;
const MAX_ROW: usize = 16 << 20;

#[derive(Clone)]
struct Control {
    flag: Arc<AtomicBool>,
    deadline: Option<Instant>,
}
impl Control {
    fn new(v: &Value, cancel: &Cancellation) -> sparkles::Result<Self> {
        let deadline = match v.get("timeout") {
            None | Some(Value::Null) => None,
            Some(ms) => Some(
                Instant::now()
                    .checked_add(Duration::from_millis(
                        ms.as_u64().filter(|m| *m > 0).ok_or_else(|| {
                            EngineError::invalid("timeout must be a positive integer")
                        })?,
                    ))
                    .ok_or_else(|| EngineError::invalid("timeout is too large"))?,
            ),
        };
        let control = Self {
            flag: cancel.flag.clone(),
            deadline,
        };
        control.check()?;
        Ok(control)
    }
    fn check(&self) -> sparkles::Result<()> {
        if self.flag.load(Ordering::Relaxed) {
            return Err(EngineError::Cancelled);
        }
        if self.deadline.is_some_and(|d| Instant::now() >= d) {
            return Err(EngineError::Timeout);
        }
        Ok(())
    }
    fn send<T>(&self, tx: &mpsc::Sender<T>, mut value: T) -> sparkles::Result<()> {
        loop {
            self.check()?;
            match tx.try_send(value) {
                Ok(()) => return Ok(()),
                Err(mpsc::error::TrySendError::Full(v)) => value = v,
                Err(mpsc::error::TrySendError::Closed(_)) => return Err(EngineError::Cancelled),
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    fn recv<T>(&self, rx: &mut mpsc::Receiver<T>) -> sparkles::Result<Option<T>> {
        loop {
            self.check()?;
            match rx.try_recv() {
                Ok(value) => return Ok(Some(value)),
                Err(mpsc::error::TryRecvError::Disconnected) => return Ok(None),
                Err(mpsc::error::TryRecvError::Empty) => {
                    std::thread::sleep(Duration::from_millis(2))
                }
            }
        }
    }
}
fn io_error(e: EngineError) -> std::io::Error {
    std::io::Error::other(e.to_string())
}
struct Input {
    rx: mpsc::Receiver<Vec<u8>>,
    bytes: std::io::Cursor<Vec<u8>>,
    control: Control,
}
impl Read for Input {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        self.control.check().map_err(io_error)?;
        loop {
            let n = self.bytes.read(out)?;
            if n > 0 {
                return Ok(n);
            }
            match self.control.recv(&mut self.rx).map_err(io_error)? {
                Some(bytes) => self.bytes = std::io::Cursor::new(bytes),
                None => return Ok(0),
            }
        }
    }
}
struct Output {
    tx: mpsc::Sender<sparkles::Result<Vec<u8>>>,
    control: Control,
    buffer: Arc<Mutex<Vec<u8>>>,
}
impl Output {
    fn flush_buffer(&self) -> std::io::Result<()> {
        let bytes = {
            let mut buffer = self.buffer.lock();
            if buffer.is_empty() {
                return Ok(());
            }
            std::mem::replace(&mut *buffer, Vec::with_capacity(CHUNK))
        };
        self.control.send(&self.tx, Ok(bytes)).map_err(io_error)
    }
}
impl Write for Output {
    fn write(&mut self, mut bytes: &[u8]) -> std::io::Result<usize> {
        let total = bytes.len();
        while !bytes.is_empty() {
            let full = {
                let mut buffer = self.buffer.lock();
                let take = bytes.len().min(CHUNK - buffer.len());
                buffer.extend_from_slice(&bytes[..take]);
                bytes = &bytes[take..];
                buffer.len() == CHUNK
            };
            if full {
                self.flush_buffer()?;
            }
        }
        Ok(total)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.control.check().map_err(io_error)?;
        self.flush_buffer()
    }
}
async fn push<T: Send>(
    sender: &Mutex<Option<mpsc::Sender<T>>>,
    value: T,
    ctl: &Control,
) -> napi::Result<()> {
    ctl.check().map_err(err)?;
    let sender = sender
        .lock()
        .clone()
        .ok_or_else(|| invalid("RDF input ended"))?;
    let request = sender.send(value);
    tokio::pin!(request);
    loop {
        ctl.check().map_err(err)?;
        tokio::select! {
            result = &mut request => return result.map_err(|_| invalid("RDF worker ended")),
            _ = tokio::time::sleep(Duration::from_millis(5)) => {}
        }
    }
}
async fn next<T>(
    receiver: &tokio::sync::Mutex<mpsc::Receiver<sparkles::Result<T>>>,
    ctl: &Control,
) -> napi::Result<Option<T>> {
    let mut receiver = receiver.lock().await;
    loop {
        if let Err(e) = ctl.check() {
            receiver.close();
            return Err(err(e));
        }
        if let Ok(value) = tokio::time::timeout(Duration::from_millis(5), receiver.recv()).await {
            // A worker reaching its deadline drops the output channel; that EOF
            // must retain the timeout rather than look like successful completion.
            ctl.check().map_err(err)?;
            return value.transpose().map_err(err);
        }
    }
}

#[napi]
pub struct NativeRdfParser {
    sender: Arc<Mutex<Option<mpsc::Sender<Vec<u8>>>>>,
    receiver: Arc<tokio::sync::Mutex<mpsc::Receiver<sparkles::Result<String>>>>,
    control: Control,
}
#[napi]
impl NativeRdfParser {
    #[napi(constructor)]
    pub fn new(options: String, cancel: &Cancellation) -> napi::Result<Self> {
        let v = parse(&options)?;
        let control = Control::new(&v, cancel).map_err(err)?;
        let fmt = streams::format(&v).map_err(err)?;
        let codec = streams::codec(&v).map_err(err)?;
        let mut parser = RdfParser::from_format(fmt);
        if let Some(base) = v["baseIri"].as_str() {
            oxrdf::NamedNode::new(base).map_err(|e| invalid(e.to_string()))?;
            parser = parser
                .with_base_iri(base)
                .map_err(|e| invalid(e.to_string()))?;
        }
        if v["lenient"].as_bool() == Some(true) {
            parser = parser.lenient();
        }
        let (sender, rx) = mpsc::channel(2);
        let (tx, receiver) = mpsc::channel(2);
        let ctl = control.clone();
        std::thread::Builder::new()
            .name("sparkles-node-rdf-parse".into())
            .spawn(move || {
                let result = (|| {
                    let input = Input {
                        rx,
                        bytes: std::io::Cursor::new(Vec::new()),
                        control: ctl.clone(),
                    };
                    let reader = codec.reader(input, None)?;
                    // Send each bounded batch immediately: a consumer can observe a
                    // quad before an upstream producer finishes its next chunk.
                    for quad in parser.for_reader(reader) {
                        ctl.check()?;
                        let quad = quad.map_err(|e| EngineError::RdfParse(e.to_string()))?;
                        let row = terms::encode_quad(&quad).to_string();
                        if row.len() > MAX_ROW {
                            return Err(EngineError::invalid(
                                "RDF quad exceeds 16 MiB transfer limit",
                            ));
                        }
                        ctl.send(&tx, Ok(row))?;
                    }
                    Ok(())
                })();
                if let Err(e) = result {
                    let _ = ctl.send(&tx, Err(ctl.check().err().unwrap_or(e)));
                }
            })
            .map_err(|e| err(e.into()))?;
        Ok(Self {
            sender: Arc::new(Mutex::new(Some(sender))),
            receiver: Arc::new(tokio::sync::Mutex::new(receiver)),
            control,
        })
    }
    #[napi]
    pub fn push<'env>(
        &self,
        env: &'env napi::Env,
        bytes: Buffer,
    ) -> napi::Result<PromiseRaw<'env, ()>> {
        if bytes.len() > CHUNK {
            return Err(invalid("RDF input chunk exceeds 64 KiB"));
        }
        let bytes = bytes.to_vec();
        let sender = self.sender.clone();
        let control = self.control.clone();
        env.spawn_future(async move { push(&sender, bytes, &control).await })
    }
    #[napi]
    pub fn end(&self) {
        self.sender.lock().take();
    }
    #[napi]
    pub fn next_quad<'env>(
        &self,
        env: &'env napi::Env,
    ) -> napi::Result<PromiseRaw<'env, Option<String>>> {
        let receiver = self.receiver.clone();
        let control = self.control.clone();
        env.spawn_future(async move { next(&receiver, &control).await })
    }
    #[napi]
    pub fn close(&self) {
        self.control.flag.store(true, Ordering::Relaxed);
        self.sender.lock().take();
    }
}
impl Drop for NativeRdfParser {
    fn drop(&mut self) {
        self.close();
    }
}

#[napi]
pub struct NativeRdfSerializer {
    sender: Arc<Mutex<Option<mpsc::Sender<oxrdf::Quad>>>>,
    receiver: Arc<tokio::sync::Mutex<mpsc::Receiver<sparkles::Result<Vec<u8>>>>>,
    control: Control,
}
#[napi]
impl NativeRdfSerializer {
    #[napi(constructor)]
    pub fn new(options: String, cancel: &Cancellation) -> napi::Result<Self> {
        let v = parse(&options)?;
        let control = Control::new(&v, cancel).map_err(err)?;
        let fmt = streams::format(&v).map_err(err)?;
        let codec = streams::codec(&v).map_err(err)?;
        let (sender, mut rx) = mpsc::channel::<oxrdf::Quad>(2);
        let (tx, receiver) = mpsc::channel(2);
        let ctl = control.clone();
        std::thread::Builder::new()
            .name("sparkles-node-rdf-serialize".into())
            .spawn(move || {
                let result = (|| {
                    let sink = Output {
                        tx: tx.clone(),
                        control: ctl.clone(),
                        buffer: Arc::new(Mutex::new(Vec::with_capacity(CHUNK))),
                    };
                    let writer = codec.writer(
                        Output {
                            tx: tx.clone(),
                            control: ctl.clone(),
                            buffer: sink.buffer.clone(),
                        },
                        None,
                        1,
                    )?;
                    let mut writer = RdfSerializer::from_format(fmt).for_writer(writer);
                    while let Some(quad) = ctl.recv(&mut rx)? {
                        writer.serialize_quad(&quad)?;
                        // Coalesce the serializer's small per-term writes, while
                        // still making a completed quad available before EOF.
                        sink.flush_buffer()?;
                    }
                    let writer = writer.finish()?;
                    writer.finish()?;
                    sink.flush_buffer()?;
                    Ok(())
                })();
                if let Err(e) = result {
                    let _ = ctl.send(&tx, Err(ctl.check().err().unwrap_or(e)));
                }
            })
            .map_err(|e| err(e.into()))?;
        Ok(Self {
            sender: Arc::new(Mutex::new(Some(sender))),
            receiver: Arc::new(tokio::sync::Mutex::new(receiver)),
            control,
        })
    }
    #[napi]
    pub fn push<'env>(
        &self,
        env: &'env napi::Env,
        quad: String,
    ) -> napi::Result<PromiseRaw<'env, ()>> {
        if quad.len() > MAX_ROW {
            return Err(invalid("RDF quad exceeds 16 MiB transfer limit"));
        }
        let quad = terms::quad(&parse(&quad)?).map_err(err)?;
        let sender = self.sender.clone();
        let control = self.control.clone();
        env.spawn_future(async move { push(&sender, quad, &control).await })
    }
    #[napi]
    pub fn end(&self) {
        self.sender.lock().take();
    }
    #[napi]
    pub fn next_bytes<'env>(
        &self,
        env: &'env napi::Env,
    ) -> napi::Result<PromiseRaw<'env, Option<Buffer>>> {
        let receiver = self.receiver.clone();
        let control = self.control.clone();
        env.spawn_future(
            async move { next(&receiver, &control).await.map(|v| v.map(Buffer::from)) },
        )
    }
    #[napi]
    pub fn close(&self) {
        self.control.flag.store(true, Ordering::Relaxed);
        self.sender.lock().take();
    }
}
impl Drop for NativeRdfSerializer {
    fn drop(&mut self) {
        self.close();
    }
}
