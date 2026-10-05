//! Rust-only environment cleanup. No JavaScript handle survives teardown.
use super::*;
use napi::Env;
use std::cell::RefCell;

type Cleanup = (Weak<()>, Box<dyn FnOnce() + Send>);
#[derive(Default)]
pub struct Resources {
    flags: Mutex<Vec<Weak<AtomicBool>>>,
    cleanup: Mutex<Vec<Cleanup>>,
}
thread_local! {
    // Keys identify environments only; they are never reused as Node-API handles.
    static ENVIRONMENTS:RefCell<HashMap<usize,Arc<Resources>>>=RefCell::new(HashMap::new());
}
pub fn resources(env: Env) -> napi::Result<Arc<Resources>> {
    let key = env.raw() as usize;
    ENVIRONMENTS.with(|environments| {
        if let Some(resources) = environments.borrow().get(&key) {
            return Ok(resources.clone());
        }
        let resources = Arc::new(Resources::default());
        env.add_env_cleanup_hook((key, resources.clone()), |(key, resources)| {
            for flag in resources.flags.lock().iter().filter_map(Weak::upgrade) {
                flag.store(true, Ordering::Relaxed)
            }
            for (marker, cleanup) in resources.cleanup.lock().drain(..) {
                if marker.strong_count() > 0 {
                    cleanup()
                }
            }
            ENVIRONMENTS.with(|environments| {
                environments.borrow_mut().remove(&key);
            });
        })?;
        environments.borrow_mut().insert(key, resources.clone());
        Ok(resources)
    })
}
impl Resources {
    pub fn flag(&self, flag: &Arc<AtomicBool>) {
        let mut flags = self.flags.lock();
        flags.retain(|f| f.strong_count() > 0);
        flags.push(Arc::downgrade(flag));
    }
    pub fn cleanup(&self, marker: &Arc<()>, cleanup: impl FnOnce() + Send + 'static) {
        let mut callbacks = self.cleanup.lock();
        callbacks.retain(|(marker, _)| marker.strong_count() > 0);
        callbacks.push((Arc::downgrade(marker), Box::new(cleanup)));
    }
}
