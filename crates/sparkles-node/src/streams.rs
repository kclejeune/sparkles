use super::*;
use napi::{
    Env,
    bindgen_prelude::{Buffer, PromiseRaw},
};
use oxrdfio::RdfParser;
use std::io::{Read, Write};
use tokio::sync::{mpsc, oneshot};

const CHUNK: usize = 64 << 10;
enum StreamResult {
    Eager(QueryResult),
    Streaming(sparkles::sparql::QueryExecution),
}
pub(crate) fn format(v: &Value) -> sparkles::Result<sparkles::io::RdfFormat> {
    sparkles::sparql::results::rdf_format_from_name(v["format"].as_str().unwrap_or("nq"))
        .ok_or_else(|| EngineError::invalid("unsupported RDF format"))
}
pub(crate) fn codec(v: &Value) -> sparkles::Result<sparkles::codec::Codec> {
    sparkles::codec::Codec::parse(v["compression"].as_str().unwrap_or("none"))
}
struct Chunks {
    rx: mpsc::Receiver<Vec<u8>>,
    bytes: std::io::Cursor<Vec<u8>>,
    flag: Arc<AtomicBool>,
}
impl Read for Chunks {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if self.flag.load(Ordering::Relaxed) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "cancelled",
            ));
        }
        loop {
            let n = self.bytes.read(out)?;
            if n > 0 {
                return Ok(n);
            }
            match self.rx.blocking_recv() {
                Some(bytes) => self.bytes = std::io::Cursor::new(bytes),
                None => return Ok(0),
            }
        }
    }
}
#[napi]
pub struct NativeUpload {
    sender: Arc<Mutex<Option<mpsc::Sender<Vec<u8>>>>>,
    answer: Arc<tokio::sync::Mutex<Option<oneshot::Receiver<sparkles::Result<String>>>>>,
    flag: Arc<AtomicBool>,
    _cleanup_marker: Arc<()>,
}
#[napi]
impl NativeUpload {
    #[napi]
    pub fn push<'env>(&self, env: &'env Env, bytes: Buffer) -> napi::Result<PromiseRaw<'env, ()>> {
        if bytes.len() > CHUNK {
            return Err(invalid("upload chunk exceeds 64 KiB"));
        }
        let bytes = bytes.to_vec();
        let sender = self
            .sender
            .lock()
            .clone()
            .ok_or_else(|| invalid("upload ended"))?;
        env.spawn_future(async move {
            sender
                .send(bytes)
                .await
                .map_err(|_| invalid("upload parser ended"))
        })
    }
    #[napi]
    pub fn finish<'env>(&self, env: &'env Env) -> napi::Result<PromiseRaw<'env, String>> {
        self.sender.lock().take();
        let answer = self.answer.clone();
        env.spawn_future(async move {
            let answer = answer
                .lock()
                .await
                .take()
                .ok_or_else(|| invalid("upload ended"))?;
            answer
                .await
                .map_err(|_| invalid("upload worker ended"))?
                .map_err(err)
        })
    }
    #[napi]
    pub fn abort(&self) {
        self.flag.store(true, Ordering::Relaxed);
        self.sender.lock().take();
    }
}
impl Drop for NativeUpload {
    fn drop(&mut self) {
        self.flag.store(true, Ordering::Relaxed);
        self.sender.lock().take();
    }
}

struct Sink {
    sender: mpsc::Sender<sparkles::Result<Vec<u8>>>,
    buffer: Vec<u8>,
}
impl Sink {
    fn flush_chunk(&mut self) -> std::io::Result<()> {
        if !self.buffer.is_empty() {
            let bytes = std::mem::replace(&mut self.buffer, Vec::with_capacity(CHUNK));
            self.sender
                .blocking_send(Ok(bytes))
                .map_err(|_| std::io::Error::new(std::io::ErrorKind::BrokenPipe, "stream closed"))?
        }
        Ok(())
    }
}
impl Write for Sink {
    fn write(&mut self, mut bytes: &[u8]) -> std::io::Result<usize> {
        let n = bytes.len();
        while !bytes.is_empty() {
            let take = bytes.len().min(CHUNK - self.buffer.len());
            self.buffer.extend_from_slice(&bytes[..take]);
            bytes = &bytes[take..];
            if self.buffer.len() == CHUNK {
                self.flush_chunk()?
            }
        }
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.flush_chunk()
    }
}
#[napi]
pub struct NativeByteStream {
    receiver: Arc<tokio::sync::Mutex<mpsc::Receiver<sparkles::Result<Vec<u8>>>>>,
    flag: Arc<AtomicBool>,
    permit: Arc<Mutex<Option<OwnedSemaphorePermit>>>,
}
#[napi]
impl NativeByteStream {
    #[napi]
    pub fn next<'env>(&self, env: &'env Env) -> napi::Result<PromiseRaw<'env, Option<Buffer>>> {
        let receiver = self.receiver.clone();
        let flag = self.flag.clone();
        let permit = self.permit.clone();
        env.spawn_future(async move {
            let mut receiver = receiver.lock().await;
            loop {
                if flag.load(Ordering::Relaxed) {
                    receiver.close();
                    permit.lock().take();
                    return Err(err(EngineError::Cancelled));
                }
                if let Ok(value) =
                    tokio::time::timeout(Duration::from_millis(10), receiver.recv()).await
                {
                    if value.as_ref().is_none_or(|value| value.is_err()) {
                        permit.lock().take();
                    }
                    return value.transpose().map(|v| v.map(Buffer::from)).map_err(err);
                }
            }
        })
    }
    #[napi]
    pub fn close(&self) {
        self.flag.store(true, Ordering::Relaxed);
        self.permit.lock().take();
        if let Ok(mut receiver) = self.receiver.try_lock() {
            receiver.close();
        }
    }
}
impl Drop for NativeByteStream {
    fn drop(&mut self) {
        self.flag.store(true, Ordering::Relaxed);
        self.permit.lock().take();
        if let Ok(mut receiver) = self.receiver.try_lock() {
            receiver.close();
        }
    }
}

#[napi]
impl NativeDataset {
    #[napi]
    pub fn load_start<'env>(
        &self,
        env: &'env Env,
        options: String,
        cancel: &Cancellation,
    ) -> napi::Result<PromiseRaw<'env, NativeUpload>> {
        let shared = self.get(true)?;
        let v = parse(&options)?;
        let flag = cancel.flag.clone();
        let resources = cancel.resources.clone();
        env.spawn_future(async move {
            let permit = permit(shared.writers.clone(), &v, &flag).await?;
            let (sender, receiver) = mpsc::channel(8);
            let (answer, reply) = oneshot::channel();
            let workerflag = flag.clone();
            std::thread::Builder::new()
                .name("sparkles-node-load".into())
                .spawn(move || {
                    let _permit = permit;
                    let result = (|| {
                        let opts = write_options(&v, workerflag.clone())?;
                        let rdf = format(&v)?;
                        let reader = Chunks {
                            rx: receiver,
                            bytes: std::io::Cursor::new(Vec::new()),
                            flag: workerflag.clone(),
                        };
                        let reader = codec(&v)?.reader(reader, None)?;
                        let mut parser = RdfParser::from_format(rdf);
                        if let Some(base) = v["baseIri"].as_str() {
                            parser = parser
                                .with_base_iri(base)
                                .map_err(|e| EngineError::invalid(e.to_string()))?
                        }
                        if v["lenient"].as_bool() == Some(true) {
                            parser = parser.lenient()
                        }
                        let graph = v["toGraph"]
                            .as_str()
                            .map(oxrdf::NamedNode::new)
                            .transpose()
                            .map_err(|e| EngineError::invalid(e.to_string()))?;
                        let (inserted, r) =
                            shared
                                .ds
                                .transaction_receipt_with(opts.clone(), move |tx| {
                                    let mut inserted = 0u64;
                                    for q in parser.for_reader(reader) {
                                        opts.check()?;
                                        let mut q =
                                            q.map_err(|e| EngineError::RdfParse(e.to_string()))?;
                                        if q.graph_name.is_default_graph()
                                            && let Some(g) = &graph
                                        {
                                            q.graph_name = g.clone().into();
                                        }
                                        inserted += tx.insert(q.as_ref())? as u64;
                                    }
                                    Ok(inserted)
                                })?;
                        Ok(
                            json!({"inserted":inserted.to_string(),"receipt":receipt(&r)})
                                .to_string(),
                        )
                    })();
                    let _ = answer.send(result);
                })
                .map_err(|e| err(e.into()))?;
            let sender = Arc::new(Mutex::new(Some(sender)));
            let weak = Arc::downgrade(&sender);
            let marker = Arc::new(());
            resources.cleanup(&marker, move || {
                if let Some(sender) = weak.upgrade() {
                    sender.lock().take();
                }
            });
            Ok(NativeUpload {
                sender,
                answer: Arc::new(tokio::sync::Mutex::new(Some(reply))),
                flag,
                _cleanup_marker: marker,
            })
        })
    }
    #[napi]
    pub async fn load_paths(
        &self,
        paths: Vec<String>,
        options: String,
        cancel: &Cancellation,
    ) -> napi::Result<String> {
        let shared = self.get(true)?;
        let v = parse(&options)?;
        let flag = cancel.flag.clone();
        let _permit = permit(shared.writers.clone(), &v, &flag).await?;
        blocking(move || {
            let graph = v["toGraph"]
                .as_str()
                .map(oxrdf::NamedNode::new)
                .transpose()
                .map_err(|e| EngineError::invalid(e.to_string()))?;
            let mut sources = Vec::new();
            for path in paths {
                let mut source = sparkles::io::Source::from_path(Path::new(&path), graph.clone())?;
                if v["format"].is_string() {
                    source.format = format(&v)?
                }
                source.lenient = v["lenient"].as_bool().unwrap_or(false);
                sources.push(source)
            }
            let r = shared.ds.store().load_with(
                &sources,
                sparkles::commit::CommitKind::Load,
                &write_options(&v, flag)?,
            )?;
            Ok(json!({"inserted":r.commit.inserted.to_string(),"receipt":receipt(&r)}).to_string())
        })
        .await
    }
    #[napi]
    pub fn dump_stream(
        &self,
        options: String,
        cancel: &Cancellation,
    ) -> napi::Result<NativeByteStream> {
        let shared = self.get(false)?;
        let v = parse(&options)?;
        let flag = cancel.flag.clone();
        let workerflag = flag.clone();
        let (sender, receiver) = mpsc::channel(2);
        std::thread::Builder::new()
            .name("sparkles-node-dump".into())
            .spawn(move || {
                let errors = sender.clone();
                let result = (|| {
                    let sink = Sink {
                        sender,
                        buffer: Vec::with_capacity(CHUNK),
                    };
                    let mut writer = codec(&v)?.writer(sink, None, 1)?;
                    let fmt = format(&v)?;
                    if let Some(graph) = v["fromGraph"].as_str() {
                        shared.ds.dump_graph(
                            oxrdf::NamedNode::new(graph)
                                .map_err(|e| EngineError::invalid(e.to_string()))?
                                .as_ref()
                                .into(),
                            &mut writer,
                            fmt,
                        )?;
                    } else {
                        shared.ds.dump(&mut writer, fmt)?;
                    }
                    if workerflag.load(Ordering::Relaxed) {
                        return Err(EngineError::Cancelled);
                    }
                    writer.finish()?;
                    Ok(())
                })();
                if let Err(e) = result {
                    let _ = errors.blocking_send(Err(e));
                }
            })
            .map_err(|e| err(e.into()))?;
        Ok(NativeByteStream {
            receiver: Arc::new(tokio::sync::Mutex::new(receiver)),
            flag,
            permit: Default::default(),
        })
    }
    #[napi]
    pub async fn dump_file(
        &self,
        path: String,
        options: String,
        cancel: &Cancellation,
    ) -> napi::Result<String> {
        let shared = self.get(false)?;
        let v = parse(&options)?;
        let flag = cancel.flag.clone();
        blocking(move || {
            let mut v = v;
            if v["format"].is_null() {
                let (f, c) = sparkles::io::format_for_path(Path::new(&path))
                    .ok_or_else(|| EngineError::invalid("cannot infer dump format"))?;
                v["format"] = f.media_type().into();
                if v["compression"].is_null() {
                    v["compression"] = c.unwrap_or(sparkles::codec::Codec::None).name().into()
                }
            }
            let f = std::fs::File::create(&path)?;
            let mut writer = codec(&v)?.writer(f, None, 1)?;
            let fmt = format(&v)?;
            let n = if let Some(graph) = v["fromGraph"].as_str() {
                let graph = oxrdf::NamedNode::new(graph)
                    .map_err(|e| EngineError::invalid(e.to_string()))?;
                shared
                    .ds
                    .dump_graph(graph.as_ref().into(), &mut writer, fmt)?
            } else {
                shared.ds.dump(&mut writer, fmt)?
            };
            if flag.load(Ordering::Relaxed) {
                return Err(EngineError::Cancelled);
            }
            writer.finish()?;
            Ok(n.to_string())
        })
        .await
    }
    #[napi]
    pub async fn query_stream(
        &self,
        text: String,
        options: String,
        cancel: &Cancellation,
    ) -> napi::Result<NativeByteStream> {
        let shared = self.get(false)?;
        let v = parse(&options)?;
        let flag = cancel.flag.clone();
        let stream_permit = permit(READERS.clone(), &v, &flag).await?;
        let result = blocking({
            let shared = shared.clone();
            let flag = flag.clone();
            let v = v.clone();
            move || match v["execution"].as_str().unwrap_or("eager") {
                "streaming" | "auto" => {
                    open_query_cursor(&shared.ds, &text, &v, flag).map(StreamResult::Streaming)
                }
                "eager" => shared
                    .ds
                    .query_with(&text, &query_options(&v, flag)?)
                    .map(StreamResult::Eager),
                _ => Err(EngineError::invalid(
                    "execution must be eager, streaming or auto",
                )),
            }
        })
        .await?;
        let (sender, receiver) = mpsc::channel(2);
        let workerflag = flag.clone();
        std::thread::Builder::new()
            .name("sparkles-node-results".into())
            .spawn(move || {
                let errors = sender.clone();
                let result = (|| {
                    let mut sink = Sink {
                        sender,
                        buffer: Vec::with_capacity(CHUNK),
                    };
                    let accept = v["accept"]
                        .as_str()
                        .unwrap_or("application/sparql-results+json");
                    match result {
                        StreamResult::Streaming(mut execution) => {
                            use sparkles::sparql::{QueryExecution, results};
                            match &mut execution {
                                QueryExecution::Eager(result) => {
                                    let result = result.result();
                                    if matches!(result.kind, QueryKind::Select | QueryKind::Ask) {
                                        let fmt = results::SolutionsFormat::from_name(accept)
                                            .ok_or_else(|| {
                                                EngineError::invalid("invalid result format")
                                            })?;
                                        results::write_solutions(result, fmt, &mut sink, None)?;
                                    } else {
                                        let fmt = results::rdf_format_from_name(accept)
                                            .ok_or_else(|| {
                                                EngineError::invalid("invalid RDF result format")
                                            })?;
                                        results::write_graph(
                                            result,
                                            fmt,
                                            &shared.ds.prefixes(),
                                            &mut sink,
                                        )?;
                                    }
                                }
                                QueryExecution::Select(cursor) => {
                                    let fmt = results::SolutionsFormat::from_name(accept)
                                        .ok_or_else(|| {
                                            EngineError::invalid("invalid result format")
                                        })?;
                                    results::write_cursor_solutions(cursor, fmt, &mut sink, None)?;
                                }
                                QueryExecution::Ask(result) => {
                                    let fmt = results::SolutionsFormat::from_name(accept)
                                        .ok_or_else(|| {
                                            EngineError::invalid("invalid result format")
                                        })?;
                                    results::write_solutions(
                                        result.result(),
                                        fmt,
                                        &mut sink,
                                        None,
                                    )?;
                                }
                                QueryExecution::Graph(cursor)
                                    if accept
                                        == results::SolutionsFormat::Sparkles.media_type() =>
                                {
                                    results::write_cursor_graph_native_json(
                                        cursor, &mut sink, None, None,
                                    )?;
                                }
                                QueryExecution::Graph(cursor) => {
                                    let fmt =
                                        results::rdf_format_from_name(accept).ok_or_else(|| {
                                            EngineError::invalid("invalid RDF result format")
                                        })?;
                                    let prefixes =
                                        shared.ds.prefixes().into_iter().collect::<Vec<_>>();
                                    sparkles::sparql::cursor::graph::write_cursor_graph(
                                        cursor, fmt, &mut sink, None, &prefixes,
                                    )?;
                                }
                            }
                        }
                        StreamResult::Eager(result) => {
                            if result.kind == QueryKind::Select || result.kind == QueryKind::Ask {
                                let fmt = sparkles::sparql::results::SolutionsFormat::from_name(
                                    accept,
                                )
                                .ok_or_else(|| EngineError::invalid("invalid result format"))?;
                                sparkles::sparql::results::write_solutions(
                                    &result, fmt, &mut sink, None,
                                )?;
                            } else {
                                let fmt = sparkles::sparql::results::rdf_format_from_name(accept)
                                    .ok_or_else(|| {
                                    EngineError::invalid("invalid RDF result format")
                                })?;
                                sparkles::sparql::results::write_graph(
                                    &result,
                                    fmt,
                                    &shared.ds.prefixes(),
                                    &mut sink,
                                )?;
                            }
                        }
                    }
                    if workerflag.load(Ordering::Relaxed) {
                        return Err(EngineError::Cancelled);
                    }
                    sink.flush()?;
                    Ok(())
                })();
                if let Err(e) = result {
                    let _ = errors.blocking_send(Err(e));
                }
            })
            .map_err(|e| err(e.into()))?;
        Ok(NativeByteStream {
            receiver: Arc::new(tokio::sync::Mutex::new(receiver)),
            flag,
            permit: Arc::new(Mutex::new(Some(stream_permit))),
        })
    }
}
