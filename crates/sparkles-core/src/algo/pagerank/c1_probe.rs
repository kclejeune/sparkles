//! Caller-specific iteration observations visible to actual Rayon workers.
use super::Control;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
#[derive(Clone, Copy)]
pub(super) enum Event {
    Iteration,
    Node(usize),
}
type Hook = Arc<dyn Fn(Event) + Send + Sync>;
static ACTIVE: AtomicBool = AtomicBool::new(false);
static HOOKS: Mutex<Vec<(Control, Hook)>> = Mutex::new(Vec::new());
pub(super) struct Scope(Control);
pub(super) fn install(ctl: &Control, hook: Hook) -> Scope {
    let mut hooks = HOOKS.lock().unwrap();
    assert!(!hooks.iter().any(|(target, _)| target.same(ctl)));
    hooks.push((ctl.clone(), hook));
    ACTIVE.store(true, Ordering::Release);
    Scope(ctl.clone())
}
impl Drop for Scope {
    fn drop(&mut self) {
        let mut hooks = HOOKS.lock().unwrap();
        hooks.retain(|(target, _)| !target.same(&self.0));
        ACTIVE.store(!hooks.is_empty(), Ordering::Release);
    }
}
pub(super) fn at(event: Event, ctl: &Control) {
    if !ACTIVE.load(Ordering::Acquire) {
        return;
    }
    let hook = HOOKS
        .lock()
        .unwrap()
        .iter()
        .find(|(target, _)| target.same(ctl))
        .map(|(_, f)| f.clone());
    // No registry lock is held while a callback waits for another worker.
    if let Some(hook) = hook {
        hook(event);
    }
}
