//! Wire types for gluon over [`strong_ipc`].
//!
//! # Message layout
//!
//! strong-ipc messages are a byte payload plus an ordered list of descriptors, with no
//! notion of a transaction code and no typed objects embedded in the bytes. gluon adds
//! both on top:
//!
//! ```text
//!   [u32 LE transaction code][payload bytes...]   + descriptors, in write order
//! ```
//!
//! Descriptors are matched to their slots positionally, exactly as binder did: the
//! schema decides how many are written and in what order, so the reader pops them in
//! the same order rather than tagging them in the byte stream. A [`Ref`] and a plain
//! `OwnedFd` are indistinguishable on the wire — both are just descriptors — so reading
//! a payload against the wrong schema yields the wrong Rust type rather than an error.
//!
//! Code `0` is the reply code used by [`ReplySender`]; generated method codes start at 8.
//!
//! # Refs and nodes
//!
//! binder had objects you host and refs to someone else's; strong-ipc's [`Ref`] and
//! [`Node`] are used here directly, unwrapped. A `Ref` is a capability to send to a node,
//! and it carries identity — strong-ipc dedupes descriptors for the same socket through a
//! registry — so two refs received in separate messages that lead to the same node
//! compare equal and hash alike, as binder's objects did.
//!
//! A `Node` deliberately holds no `Ref` to itself, so [`Node::new`] hands both back and
//! generated [`RefExt`] impls pass the ref straight into a proxy. Keep the node:
//! dropping it hangs its socket up and every ref to it goes dead.
//!
//! One thing binder had that a bare `Ref` cannot say is which side of an object you are
//! on. A generated proxy is a `Ref` and nothing else, so a node this process built and one
//! a peer handed us are the same type — right for the wire, lossy for the builder, who
//! knew the handler and then had nowhere to put it. [`LocalRef`] is that place: the proxy
//! plus the `Arc<H>` behind it, handed back by [`RefExt::new_node`] and
//! [`RefExt::new_service`], and recovered from a ref that comes back in by
//! [`RefExt::local_from_ref`]. It is an ergonomic pairing only — nothing about it reaches
//! the wire, where a `Ref` is still just a `Ref`.

mod codegen_info;
mod data;
mod liveness;
pub mod primitive_impls;
mod refs;
mod reply;
pub use gluon_ipc_derive::Handler;
pub use strong_ipc::{
	DeathNotifier, FdVec, Handler, MAX_MESSAGE_SIZE, Message, Node, NodeError, Ref, RefFsBinding,
	UCred,
};

pub use codegen_info::*;
pub use data::*;
pub use liveness::*;
pub use refs::*;
pub use reply::*;

use rustix::process::{RawGid, RawPid, RawUid};
use std::sync::Arc;
use strong_ipc::TrySendError;
use thiserror::Error;

use crate::sealed::Sealed;

pub trait Convertable: 'static + Sized {
	fn write(&self, data: &mut DataBuilder) -> Result<(), WriteError>;
	fn write_owned(self, data: &mut DataBuilder) -> Result<(), WriteError>;
	fn read(data: &mut DataReader) -> Result<Self, ReadError>;
}

/// Anything that can produce the ref that reaches it.
pub trait ToRef: Send + Sync + 'static {
	fn to_ref(&self) -> Ref;
}
impl ToRef for Ref {
	fn to_ref(&self) -> Ref {
		self.clone()
	}
}

pub trait Interface: ToRef {
	const ID: &'static str;
}

mod sealed {
	pub trait Sealed {}
	impl<T: super::Interface> Sealed for T {}
	impl<T: super::Interface> Sealed for Option<T> {}
}
/// A trait implemented for T: Interface and Option<T: Interface>.
pub trait OptionalInterfaceRef: Sealed {
	type InnerInterface: Interface + RefExt;
	const OPTIONAL: bool;
}
impl<I: Interface + RefExt> OptionalInterfaceRef for I {
	type InnerInterface = I;
	const OPTIONAL: bool = false;
}
impl<I: Interface + RefExt> OptionalInterfaceRef for Option<I> {
	type InnerInterface = I;
	const OPTIONAL: bool = true;
}

/// A handler, or a share of one already in an `Arc`.
///
/// Deliberately not `Into<Arc<H>>`, which is ambiguous for the case that matters:
/// `Arc<H>: Into<Arc<?H>>` matches both `From<T> for T` and `From<T> for Arc<T>`, so
/// passing an `Arc` leaves `?H` unpinned and the call needs a turbofish. The two impls
/// here differ in the trait's own parameter rather than only in `Self`, and the reflexive
/// reading of the `Arc` case (`H = Arc<H>`) fails its `Handler` bound, so exactly one
/// candidate survives and `H` falls out of the argument on its own.
pub trait IntoHandler<H: Handler> {
	fn into_handler(self) -> Arc<H>;
}
impl<H: Handler> IntoHandler<H> for H {
	fn into_handler(self) -> Arc<H> {
		Arc::new(self)
	}
}
impl<H: Handler> IntoHandler<H> for Arc<H> {
	fn into_handler(self) -> Arc<H> {
		self
	}
}

/// An interface `H` can answer the methods of.
///
/// Generated per interface as `impl<H: TestHandler> HandledBy<H> for Test {}`, which is
/// what carries the per-interface bound now that [`RefExt`] is not generic over the
/// handler. That matters because [`RefExt::connect`] mentions no handler at all — with the
/// bound on the trait, `Test::connect(path)` would have had nothing to infer it from.
///
/// The interface is `Self` and the handler is the parameter, not the other way around,
/// because the orphan rule needs the local type first: an `impl<H: TestHandler>
/// HandlerFor<Test> for H` leaves `H` uncovered ahead of any local type and is refused
/// outright.
pub trait HandledBy<H: Handler>: Interface {}

/// Sends `data` to `target` under `code`.
///
/// Never blocks: this is [`Ref::try_send`], so a peer that is merely behind reports
/// [`SendError::Full`] rather than parking the caller.
pub fn transact(target: &Ref, code: u32, data: DataBuilder) -> Result<(), SendError> {
	target.try_send(data.finish(code)).map_err(SendError::from)
}

#[derive(Debug, Error)]
pub enum SendError {
	#[error("Failed to write Parameters: {0}")]
	ParamWriteError(#[from] WriteError),
	#[error("Failed to read return values: {0}")]
	ReturnReadError(#[from] ReadError),
	/// The peer is alive but behind: its socket buffer and outbound queue are both full.
	///
	/// binder had no equivalent — it blocked instead. gluon's one-way sends never block,
	/// so backpressure surfaces here and the message was **not** delivered.
	#[error("The peer's outbound queue is full")]
	Full,
	#[error("The peer is gone")]
	Closed,
	#[error("Payload is {size} bytes, over the {MAX_MESSAGE_SIZE} byte limit")]
	TooLarge { size: usize },
	#[error("Could not create the reply object: {0}")]
	Node(#[from] NodeError),
}
impl From<TrySendError> for SendError {
	fn from(err: TrySendError) -> Self {
		match err {
			TrySendError::Full(_) => SendError::Full,
			TrySendError::TooLarge(m) => SendError::TooLarge {
				size: m.data().len(),
			},
			TrySendError::Closed(_) => SendError::Closed,
		}
	}
}

/// Who sent the transaction being handled, as reported by the kernel.
///
/// The credentials are an `Option` because `SCM_CREDENTIALS` is what supplies them.
/// strong-ipc sets `SO_PASSCRED` on every socket it receives on, so in practice they are
/// always present — but a peer not going through this crate is not obliged to cooperate,
/// and a handler that gates on identity should treat `None` as "unknown", never as
/// "trusted".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Context {
	creds: Option<UCred>,
}

impl Context {
	pub fn new(creds: Option<UCred>) -> Self {
		Self { creds }
	}
	pub fn creds(&self) -> Option<UCred> {
		self.creds
	}
	pub fn sender_pid(&self) -> Option<RawPid> {
		self.creds.map(|c| c.pid.as_raw_nonzero().get())
	}
	pub fn sender_uid(&self) -> Option<RawUid> {
		self.creds.map(|c| c.uid.as_raw())
	}
	pub fn sender_gid(&self) -> Option<RawGid> {
		self.creds.map(|c| c.gid.as_raw())
	}
}

#[cfg(test)]
mod tests {
	use std::marker::PhantomData;

	// The derive emits `gluon_ipc::` paths; alias this crate so those paths
	// resolve when the tests are compiled as part of `gluon-ipc` itself.
	extern crate self as gluon_ipc;

	use super::*;

	fn assert_handler<T: Handler>() {}

	// --- plain struct ---

	#[derive(Debug, Handler)]
	struct PlainHandler;

	impl PlainHandler {
		async fn dispatch_one_way(
			&self,
			_code: u32,
			_data: DataReader,
			_ctx: Context,
		) -> Result<(), SendError> {
			Ok(())
		}
	}

	// --- generic struct (bounds on type param) ---

	#[derive(Debug, Handler)]
	struct GenericHandler<T: std::fmt::Debug + Send + Sync + 'static>(PhantomData<T>);

	impl<T: std::fmt::Debug + Send + Sync + 'static> GenericHandler<T> {
		async fn dispatch_one_way(
			&self,
			_code: u32,
			_data: DataReader,
			_ctx: Context,
		) -> Result<(), SendError> {
			Ok(())
		}
	}

	// --- generic struct (bounds in where clause) ---

	#[derive(Debug, Handler)]
	struct WhereHandler<T>(PhantomData<T>)
	where
		T: std::fmt::Debug + Send + Sync + 'static;

	impl<T> WhereHandler<T>
	where
		T: std::fmt::Debug + Send + Sync + 'static,
	{
		async fn dispatch_one_way(
			&self,
			_code: u32,
			_data: DataReader,
			_ctx: Context,
		) -> Result<(), SendError> {
			Ok(())
		}
	}

	#[test]
	fn plain_handler_is_handler() {
		assert_handler::<PlainHandler>();
	}

	#[test]
	fn generic_handler_is_handler() {
		assert_handler::<GenericHandler<u32>>();
	}

	#[test]
	fn where_clause_handler_is_handler() {
		assert_handler::<WhereHandler<String>>();
	}

	#[test]
	fn code_and_payload_round_trip() {
		let mut builder = DataBuilder::new();
		builder.write_u32(7).unwrap();
		builder.write_str("hello").unwrap();
		let message = builder.finish(12);

		let (code, mut reader) = DataReader::from_wire(message.data(), FdVec::new()).unwrap();
		assert_eq!(code, 12);
		assert_eq!(reader.read_u32().unwrap(), 7);
		assert_eq!(reader.read_string().unwrap(), "hello");
	}

	#[test]
	fn short_message_has_no_code() {
		assert!(matches!(
			DataReader::from_wire(&[0, 1], FdVec::new()),
			Err(ReadError::NotEnoughBytes)
		));
	}

	#[test]
	fn missing_descriptor_is_an_error() {
		let message = DataBuilder::new().finish(0);
		let (_, mut reader) = DataReader::from_wire(message.data(), FdVec::new()).unwrap();
		assert!(matches!(
			reader.read_fd(),
			Err(ReadError::MissingDescriptor)
		));
	}
}
