use leptos::prelude::*;

/// Sets a signal only when the value differs, so polling does not rebuild unchanged views.
pub fn set_if_changed<T>(signal: RwSignal<T>, value: T)
where
    T: PartialEq + Send + Sync + 'static,
{
    if signal.try_with_untracked(|current| *current != value) == Some(true) {
        signal.set(value);
    }
}
