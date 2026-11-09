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
//!
//! # Bufs
//!
//! A message tops out at [`MAX_MESSAGE_SIZE`], so anything bigger goes in a [`Buf`]: a
//! regular file sent by fd along with how many bytes of it count. A plain `Buf` is raw
//! bytes, a `Buf<T>` holds a `T` encoded the same way a message payload is.
//!
//! ```text
//!   interface Canvas {
//!       set_lines(lines: Buf<Vec<Line>>) -> ()
//!       set_keymap(keymap: Buf) -> ()
//!   }
//! ```
//!
//! On the wire it's a `u64` length in the bytes and the fd in the descriptors. Receiving
//! one checks the fd is a regular file, open for reading, and at least that long, so a
//! pipe or a socket can't sneak in. Any such file works, a memfd from [`Buf::new`], a file
//! on disk, or whatever [`Buf::from_fd`] is handed.
//!
//! The contents have to be [`Bufferable`], meaning no fds or refs anywhere inside, since
//! those only exist alongside the message that carried them. Codegen works that out from
//! the protocol and refuses a `Buf<T>` that breaks it.
//!
//! Nothing is sealed, so the sender can rewrite or truncate the file whenever it likes.
//! Reads go through `pread` rather than mmap, so that can only ever give you garbage or a
//! [`BufError::TooShort`], never a crash. The length is the sender's claim and the file can
//! be huge and sparse, so look at [`Buf::len`] before decoding one from a peer you don't
//! trust.
//!
//! ## Live bufs and the lease
//!
//! [`BufWriter`] keeps one memfd and rewrites it on every [`BufWriter::set`]. Share it
//! once, and after that a peer only needs the new length for [`Buf::set_len`], or a fresh
//! [`BufWriter::share`] if that's simpler. [`Buf::decode_into`] on the other end reuses
//! the old value's allocations, so a buf updated every frame stops allocating.
//!
//! The catch is that a write and a read can overlap and tear. The fix is a lease: a
//! method taking a buf you'll rewrite returns `()` instead of being oneway, the handler
//! decodes before it returns, and the sender waits for that reply before the next `set`.
//! The reply then means "done reading", and no locking is needed.
//!
//! ```ignore
//! let mut w = BufWriter::new()?;
//! w.set(&lines)?;
//! canvas.set_lines(w.share()?).await?;
//! // the handler has decoded it once the reply arrives, so rewriting is safe now
//! w.set(&more_lines)?;
//! canvas.set_lines(w.share()?).await?;
//! ```

mod buf;
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

pub use buf::*;
pub use codegen_info::*;
pub use data::*;
pub use liveness::*;
pub use refs::*;
pub use reply::*;

use rustix::process::{RawGid, RawPid, RawUid};
use std::sync::Arc;
use strong_ipc::TrySendError;
use thiserror::Error;

pub trait Convertable: 'static + Sized {
	fn write(&self, data: &mut DataBuilder) -> Result<(), WriteError>;
	fn write_owned(self, data: &mut DataBuilder) -> Result<(), WriteError>;
	fn read(data: &mut DataReader) -> Result<Self, ReadError>;

	/// [`Convertable::read`] over a value you already have, reusing whatever it allocated, so
	/// decoding the same kind of thing over and over (like a buf that keeps getting updated)
	/// settles into not allocating at all
	///
	/// on error `self` is left partly overwritten, still a valid value but a mix of old and new
	fn read_into(&mut self, data: &mut DataReader) -> Result<(), ReadError> {
		*self = Self::read(data)?;
		Ok(())
	}

	/// how many bytes [`Convertable::write`] will add, so space can be set aside before
	/// encoding instead of growing into it. fds and refs travel beside the bytes and count
	/// for nothing
	fn encoded_len(&self) -> usize;
}

/// plain data that can live in a buf, so nothing that carries an fd or ref since those
/// only exist in the message they were sent with
pub trait Bufferable: Convertable {}

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

	fn forged_len() -> DataReader {
		let mut builder = DataBuilder::new();
		builder.write_u32(u32::MAX).unwrap();
		builder.write_u64(7).unwrap();
		let message = builder.finish(0);
		let (_, reader) = DataReader::from_wire(message.data(), FdVec::new()).unwrap();
		reader
	}

	fn assert_len<T: Convertable>(v: &T) {
		let mut builder = DataBuilder::new();
		v.write(&mut builder).unwrap();
		assert_eq!(v.encoded_len(), builder.finish(0).data().len() - 4);
	}

	#[test]
	fn encoded_len_matches_encoding() {
		assert_len(&true);
		assert_len(&1u8);
		assert_len(&-1i8);
		assert_len(&-2i16);
		assert_len(&-3i32);
		assert_len(&-4i64);
		assert_len(&1.5f32);
		assert_len(&2.5f64);
		assert_len(&u16::MAX);
		assert_len(&u32::MAX);
		assert_len(&u64::MAX);
		assert_len(&String::from("hello"));
		assert_len(&String::new());
		assert_len(&vec![String::from("a"), String::from("bcd")]);
		assert_len(&Vec::<u64>::new());
		assert_len(&[String::from("x"), String::from("yz")]);
		assert_len(&Some(vec![1u32, 2]));
		assert_len(&None::<String>);
		assert_len(&Ok::<_, String>(7u16));
		assert_len(&Err::<u16, _>(String::from("no")));
		assert_len(&Box::new(String::from("boxed")));
		assert_len(&std::collections::HashMap::from([(
			1u8,
			String::from("one"),
		)]));
		assert_len(&std::collections::HashSet::from([
			String::from("s"),
			String::from("t"),
		]));
		let (r, _w) = std::io::pipe().unwrap();
		assert_len(&vec![std::os::fd::OwnedFd::from(r)]);
		assert_len(&Buf::from_bytes(b"abc").unwrap());
	}

	#[test]
	fn bare_bytes_round_trip() {
		let mut builder = DataBuilder::bare();
		"hello".to_string().write(&mut builder).unwrap();
		7u32.write(&mut builder).unwrap();
		let bytes = builder.into_bytes().unwrap();
		assert_eq!(bytes.len(), 4 + 5 + 4);

		let mut reader = DataReader::from_bytes(bytes);
		assert_eq!(String::read(&mut reader).unwrap(), "hello");
		assert_eq!(u32::read(&mut reader).unwrap(), 7);
		assert!(matches!(
			u8::read(&mut reader),
			Err(ReadError::NotEnoughBytes)
		));
	}

	#[test]
	fn bare_buffers_are_reused() {
		let mut reader = DataReader::from_bytes(Vec::with_capacity(1024));
		let _ = u8::read(&mut reader);
		let bytes = reader.into_bytes();
		let cap = bytes.capacity();

		let mut builder = DataBuilder::bare_in(bytes);
		7u64.write(&mut builder).unwrap();
		let bytes = builder.into_bytes().unwrap();
		assert_eq!(bytes, 7u64.to_le_bytes());
		assert_eq!(bytes.capacity(), cap);
	}

	#[test]
	fn descriptors_cant_be_bare() {
		let (fd, _) = std::io::pipe().unwrap();
		let mut builder = DataBuilder::bare();
		builder.write_owned_fd(fd.into()).unwrap();
		assert!(matches!(
			builder.into_bytes(),
			Err(WriteError::DescriptorsInBytes)
		));
	}

	fn read_over<T: Convertable>(target: &mut T, v: &T) {
		let mut builder = DataBuilder::bare();
		v.write(&mut builder).unwrap();
		let mut reader = DataReader::from_bytes(builder.into_bytes().unwrap());
		target.read_into(&mut reader).unwrap();
	}

	#[test]
	fn read_into_reuses_nested_allocations() {
		let mut target = vec![Vec::<u32>::with_capacity(64), Vec::with_capacity(64)];
		let outer = target.as_ptr();
		let inner = target[0].as_ptr();

		read_over(&mut target, &vec![vec![1, 2, 3], vec![4]]);
		assert_eq!(target, vec![vec![1, 2, 3], vec![4]]);
		assert_eq!(target.as_ptr(), outer);
		assert_eq!(target[0].as_ptr(), inner);
	}

	#[test]
	fn read_into_grows_and_shrinks() {
		let mut target = vec![1u32, 2];
		read_over(&mut target, &vec![5, 6, 7, 8]);
		assert_eq!(target, vec![5, 6, 7, 8]);
		read_over(&mut target, &vec![9]);
		assert_eq!(target, vec![9]);
	}

	#[test]
	fn read_into_reuses_strings() {
		let mut target = String::with_capacity(64);
		let p = target.as_ptr();
		read_over(&mut target, &"hello".to_string());
		assert_eq!(target, "hello");
		assert_eq!(target.as_ptr(), p);
	}

	#[test]
	fn arrays_have_no_length() {
		let v = [String::from("a"), String::from("bc")];
		let mut builder = DataBuilder::bare();
		v.write(&mut builder).unwrap();
		let bytes = builder.into_bytes().unwrap();
		assert_eq!(bytes.len(), 4 + 1 + 4 + 2);
		let mut reader = DataReader::from_bytes(bytes);
		assert_eq!(<[String; 2]>::read(&mut reader).unwrap(), v);

		let mut target = [String::with_capacity(64), String::with_capacity(64)];
		let p = target[1].as_ptr();
		read_over(&mut target, &v);
		assert_eq!((&target, target[1].as_ptr()), (&v, p));
	}

	#[test]
	fn read_into_switches_variants() {
		let mut target = Some(vec![1u32]);
		read_over(&mut target, &None);
		assert_eq!(target, None);
		read_over(&mut target, &Some(vec![2]));
		assert_eq!(target, Some(vec![2]));

		let mut target: Result<u32, String> = Ok(1);
		read_over(&mut target, &Err("no".into()));
		assert_eq!(target, Err("no".into()));
		read_over(&mut target, &Err("again".into()));
		assert_eq!(target, Err("again".into()));
	}

	#[test]
	fn read_into_replaces_sets_and_maps() {
		use std::collections::{HashMap, HashSet};
		let mut set = HashSet::from([1u32, 2, 3]);
		read_over(&mut set, &HashSet::from([4]));
		assert_eq!(set, HashSet::from([4]));

		let mut map = HashMap::from([(1u32, "a".to_string())]);
		read_over(&mut map, &HashMap::from([(2, "b".to_string())]));
		assert_eq!(map, HashMap::from([(2, "b".to_string())]));
	}

	#[test]
	fn forged_lengths_dont_preallocate() {
		assert!(matches!(
			Vec::<u64>::read(&mut forged_len()),
			Err(ReadError::NotEnoughBytes)
		));
		assert!(matches!(
			std::collections::HashSet::<u64>::read(&mut forged_len()),
			Err(ReadError::NotEnoughBytes)
		));
		assert!(matches!(
			std::collections::HashMap::<u64, u64>::read(&mut forged_len()),
			Err(ReadError::NotEnoughBytes)
		));
	}
}
