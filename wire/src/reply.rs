use strong_ipc::{FdVec, Handler, Node, Ref, UCred};
use tokio::sync::mpsc;

use crate::{DataBuilder, DataReader, SendError, WriteError, transact};


/// The reply-carrying transaction code. Generated method codes start at 8, so this can
/// never collide with one.
pub const REPLY_CODE: u32 = 0;

/// Handle to reply to a call whose return value is being sent back asynchronously,
/// separately from the `_oneway` dispatch future completing. Call `send(value)` with
/// the same value the corresponding non-`_oneway` method would have returned; `encode`
/// (supplied by codegen when the sender is constructed) knows how to convert and write
/// that value onto the wire, so callers never touch a `DataBuilder` directly.
pub struct ReplySender<T> {
	callback: Ref,
	encode: fn(T, &mut DataBuilder) -> Result<(), WriteError>,
}

impl<T> std::fmt::Debug for ReplySender<T> {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("ReplySender").finish_non_exhaustive()
	}
}

impl<T> ReplySender<T> {
	pub fn new(callback: Ref, encode: fn(T, &mut DataBuilder) -> Result<(), WriteError>) -> Self {
		Self { callback, encode }
	}

	pub fn send(self, value: T) -> Result<(), SendError> {
		let mut payload = DataBuilder::new();
		(self.encode)(value, &mut payload)?;
		transact(&self.callback, REPLY_CODE, payload)
	}
}
/// A wrapper for the throwaway node a proxy hands out to receive one reply.
pub struct ReturnReceiver(Node<ReturnHandler>, mpsc::Receiver<DataReader>);
impl ReturnReceiver {
	pub fn new() -> Result<(Self, Ref), SendError> {
		let (handler, recv) = ReturnHandler::new();
		let (node, node_ref) = Node::new(handler)?;
		Ok((Self(node, recv), node_ref))
	}
	pub async fn recv(&mut self) -> Result<DataReader, SendError> {
		tokio::select! {
			// this unwrap should be fine since we
			v = self.1.recv() => { Ok(v.unwrap()) }
			_ = self.0.death_notification() => { Err(SendError::Closed) }
		}
	}
}

/// The handler behind the throwaway node a proxy hands out to receive one reply.
struct ReturnHandler(mpsc::Sender<DataReader>);

impl std::fmt::Debug for ReturnHandler {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("ReturnHandler").finish()
	}
}
impl Handler for ReturnHandler {
	async fn handle(&self, data: &mut [u8], fds: FdVec, _creds: Option<UCred>) {
		let Ok((_code, reader)) = DataReader::from_wire(data, fds) else {
			return;
		};
		_ = self.0.send(reader).await;
	}
}
impl ReturnHandler {
	pub fn new() -> (Self, mpsc::Receiver<DataReader>) {
		let (tx, rx) = mpsc::channel(1);
		(Self(tx), rx)
	}
}
