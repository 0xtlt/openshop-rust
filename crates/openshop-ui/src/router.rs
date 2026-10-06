//! Programmatic navigation, available outside the reactive scope of the page that started it.

use std::{cell::RefCell, rc::Rc};

type Navigate = Rc<dyn Fn(&str)>;

thread_local! {
    static NAVIGATE: RefCell<Option<Navigate>> = const { RefCell::new(None) };
}

pub(crate) fn install(navigate: impl Fn(&str) + 'static) {
    NAVIGATE.with(|slot| *slot.borrow_mut() = Some(Rc::new(navigate)));
}

/// Navigates to an embedded UI path such as `/flows` or `/reviews/42` without reloading.
pub fn route(path: &str) {
    let navigate = NAVIGATE.with(|slot| slot.borrow().clone());
    if let Some(navigate) = navigate {
        navigate(path);
    }
}
