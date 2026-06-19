//! External connectors: long-running adapters that consume [`Core`] and bridge it
//! to outside services. Like `tui` and `server`, a connector depends on the core
//! but the core knows nothing about it.
//!
//! Today this is push-only Telegram (alerts on newly-ingested articles). A future
//! `Connector` trait would be introduced here once a second connector exists; with
//! only one, a concrete module is simpler than a premature abstraction.
//!
//! [`Core`]: crate::core::Core

pub mod telegram;
