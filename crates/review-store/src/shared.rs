//! A shared handle to the caller's single Store connection. Task execution, domain receipts
//! and lease renewal serialize through this handle; cloning it never opens another writer.

use std::ops::Deref;
use std::sync::{Arc, Mutex};

use crate::EventStore;

#[derive(Clone)]
pub struct SharedEventStore<'a>(Arc<Mutex<&'a mut EventStore>>);

impl<'a> SharedEventStore<'a> {
    pub fn new(store: &'a mut EventStore) -> Self {
        Self(Arc::new(Mutex::new(store)))
    }
}

impl<'a> Deref for SharedEventStore<'a> {
    type Target = Mutex<&'a mut EventStore>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
