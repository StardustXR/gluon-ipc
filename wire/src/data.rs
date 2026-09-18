use std::{os::fd::{BorrowedFd, OwnedFd}, string::FromUtf8Error};

use strong_ipc::{FdVec, Message, Ref};
use thiserror::Error;

use crate::ToRef;

/// A descriptor waiting to go out on a message.
///
/// [`Message`] only accepts an `OwnedFd` or a `Ref`, so a borrowed fd is duplicated when
/// it is written. The kernel dups again out of `SCM_RIGHTS` at send time either way, so
/// this costs one extra `dup` on the borrowed path and nothing on the owned one.
enum Attachment {
	Fd(OwnedFd),
	Ref(Ref),
}

pub struct DataBuilder {
	/// starts with four zero bytes reserved for the transaction code, patched in by
	/// [`DataBuilder::finish`] so the code never costs a second buffer or a copy
	data: Vec<u8>,
	fds: Vec<Attachment>,
}

pub struct DataReader {
	/// owned rather than borrowed from the receive buffer: a [`DataReader`] outlives the
	/// handler call when it travels through [`ReturnHandler`]'s channel to a waiting
	/// proxy method
	data: Vec<u8>,
	cursor: usize,
	fds: std::vec::IntoIter<OwnedFd>,
}

impl Default for DataBuilder {
	fn default() -> Self {
		Self::new()
	}
}
impl DataBuilder {
	pub fn new() -> Self {
		Self {
			data: vec![0; size_of::<u32>()],
			fds: Vec::new(),
		}
	}
	/// Seals this payload into a message carrying `code`.
	pub fn finish(self, code: u32) -> Message {
		let Self { mut data, fds } = self;
		data[..size_of::<u32>()].copy_from_slice(&code.to_le_bytes());
		let mut message = Message::from_data(data);
		for fd in fds {
			match fd {
				Attachment::Fd(fd) => message.add_fd(fd),
				Attachment::Ref(node_ref) => message.add_ref(&node_ref),
			}
		}
		message
	}
}
impl DataReader {
	/// Splits a received message into its transaction code and a reader over the rest.
	pub fn from_wire(data: &[u8], fds: FdVec) -> Result<(u32, Self), ReadError> {
		let code = data
			.get(..size_of::<u32>())
			.and_then(|b| b.try_into().ok())
			.map(u32::from_le_bytes)
			.ok_or(ReadError::NotEnoughBytes)?;
		Ok((
			code,
			Self {
				data: data.to_vec(),
				cursor: size_of::<u32>(),
				fds: fds.into_vec().into_iter(),
			},
		))
	}

	fn read_bytes(&mut self, len: usize) -> Result<&[u8], ReadError> {
		let end = self
			.cursor
			.checked_add(len)
			.ok_or(ReadError::NotEnoughBytes)?;
		let bytes = self
			.data
			.get(self.cursor..end)
			.ok_or(ReadError::NotEnoughBytes)?;
		self.cursor = end;
		Ok(bytes)
	}
}

impl DataBuilder {
	pub fn write_str(&mut self, str: &str) -> Result<(), WriteError> {
		if str.len() > u32::MAX as usize {
			return Err(WriteError::StringToLong);
		}
		self.write_u32(str.len() as u32)?;
		self.data.extend_from_slice(str.as_bytes());
		Ok(())
	}
	pub fn write_f64(&mut self, float: f64) -> Result<(), WriteError> {
		self.data.extend_from_slice(&float.to_le_bytes());
		Ok(())
	}
	pub fn write_f32(&mut self, float: f32) -> Result<(), WriteError> {
		self.data.extend_from_slice(&float.to_le_bytes());
		Ok(())
	}
	pub fn write_bool(&mut self, bool: bool) -> Result<(), WriteError> {
		self.write_u8(bool as u8)?;
		Ok(())
	}
	/// Duplicates `fd` — see [`Attachment`]. Prefer [`DataBuilder::write_owned_fd`].
	pub fn write_fd(&mut self, fd: BorrowedFd<'_>) -> Result<(), WriteError> {
		let fd = fd.try_clone_to_owned().map_err(WriteError::DupFd)?;
		self.fds.push(Attachment::Fd(fd));
		Ok(())
	}
	pub fn write_owned_fd(&mut self, fd: OwnedFd) -> Result<(), WriteError> {
		self.fds.push(Attachment::Fd(fd));
		Ok(())
	}
	pub fn write_ref(&mut self, node_ref: &impl ToRef) -> Result<(), WriteError> {
		self.fds.push(Attachment::Ref(node_ref.to_ref()));
		Ok(())
	}
}

// the ints
impl DataBuilder {
	pub fn write_u64(&mut self, int: u64) -> Result<(), WriteError> {
		self.data.extend_from_slice(&int.to_le_bytes());
		Ok(())
	}
	pub fn write_i64(&mut self, int: i64) -> Result<(), WriteError> {
		self.data.extend_from_slice(&int.to_le_bytes());
		Ok(())
	}
	pub fn write_u32(&mut self, int: u32) -> Result<(), WriteError> {
		self.data.extend_from_slice(&int.to_le_bytes());
		Ok(())
	}
	pub fn write_i32(&mut self, int: i32) -> Result<(), WriteError> {
		self.data.extend_from_slice(&int.to_le_bytes());
		Ok(())
	}
	pub fn write_u16(&mut self, int: u16) -> Result<(), WriteError> {
		self.data.extend_from_slice(&int.to_le_bytes());
		Ok(())
	}
	pub fn write_i16(&mut self, int: i16) -> Result<(), WriteError> {
		self.data.extend_from_slice(&int.to_le_bytes());
		Ok(())
	}
	pub fn write_u8(&mut self, int: u8) -> Result<(), WriteError> {
		self.data.extend_from_slice(&int.to_le_bytes());
		Ok(())
	}
	pub fn write_i8(&mut self, int: i8) -> Result<(), WriteError> {
		self.data.extend_from_slice(&int.to_le_bytes());
		Ok(())
	}
}
#[derive(Debug, Error)]
pub enum WriteError {
	#[error("String is longer than u32::MAX bytes")]
	StringToLong,
	#[error("List is longer than u32::MAX items")]
	ListToLong,
	#[error("Could not duplicate borrowed fd: {0}")]
	DupFd(#[source] std::io::Error),
}

impl DataReader {
	pub fn read_string(&mut self) -> Result<String, ReadError> {
		let len = self.read_u32()?;
		let data = self.read_bytes(len as usize)?;
		Ok(String::from_utf8(data.to_vec())?)
	}
	pub fn read_f64(&mut self) -> Result<f64, ReadError> {
		let bytes = self.read_bytes(size_of::<f64>())?;
		Ok(f64::from_le_bytes(
			bytes.try_into().map_err(|_| ReadError::NotEnoughBytes)?,
		))
	}
	pub fn read_f32(&mut self) -> Result<f32, ReadError> {
		let bytes = self.read_bytes(size_of::<f32>())?;
		Ok(f32::from_le_bytes(
			bytes.try_into().map_err(|_| ReadError::NotEnoughBytes)?,
		))
	}
	pub fn read_bool(&mut self) -> Result<bool, ReadError> {
		Ok(self.read_u8()? != 0)
	}
	pub fn read_fd(&mut self) -> Result<OwnedFd, ReadError> {
		self.fds.next().ok_or(ReadError::MissingDescriptor)
	}
	pub fn read_ref(&mut self) -> Result<Ref, ReadError> {
		self.read_fd().map(Ref::from_owned_fd)
	}
}

// the ints
impl DataReader {
	pub fn read_u64(&mut self) -> Result<u64, ReadError> {
		let bytes = self.read_bytes(size_of::<u64>())?;
		Ok(u64::from_le_bytes(
			bytes.try_into().map_err(|_| ReadError::NotEnoughBytes)?,
		))
	}
	pub fn read_i64(&mut self) -> Result<i64, ReadError> {
		let bytes = self.read_bytes(size_of::<i64>())?;
		Ok(i64::from_le_bytes(
			bytes.try_into().map_err(|_| ReadError::NotEnoughBytes)?,
		))
	}
	pub fn read_u32(&mut self) -> Result<u32, ReadError> {
		let bytes = self.read_bytes(size_of::<u32>())?;
		Ok(u32::from_le_bytes(
			bytes.try_into().map_err(|_| ReadError::NotEnoughBytes)?,
		))
	}
	pub fn read_i32(&mut self) -> Result<i32, ReadError> {
		let bytes = self.read_bytes(size_of::<i32>())?;
		Ok(i32::from_le_bytes(
			bytes.try_into().map_err(|_| ReadError::NotEnoughBytes)?,
		))
	}
	pub fn read_u16(&mut self) -> Result<u16, ReadError> {
		let bytes = self.read_bytes(size_of::<u16>())?;
		Ok(u16::from_le_bytes(
			bytes.try_into().map_err(|_| ReadError::NotEnoughBytes)?,
		))
	}
	pub fn read_i16(&mut self) -> Result<i16, ReadError> {
		let bytes = self.read_bytes(size_of::<i16>())?;
		Ok(i16::from_le_bytes(
			bytes.try_into().map_err(|_| ReadError::NotEnoughBytes)?,
		))
	}
	pub fn read_u8(&mut self) -> Result<u8, ReadError> {
		let bytes = self.read_bytes(size_of::<u8>())?;
		Ok(u8::from_le_bytes(
			bytes.try_into().map_err(|_| ReadError::NotEnoughBytes)?,
		))
	}
	pub fn read_i8(&mut self) -> Result<i8, ReadError> {
		let bytes = self.read_bytes(size_of::<i8>())?;
		Ok(i8::from_le_bytes(
			bytes.try_into().map_err(|_| ReadError::NotEnoughBytes)?,
		))
	}
}

#[derive(Debug, Error)]
pub enum ReadError {
	#[error("Not enough bytes for type")]
	NotEnoughBytes,
	#[error("Message carried fewer descriptors than the schema expects")]
	MissingDescriptor,
	#[error("String data is not valid utf8: {0}")]
	StringNotUtf8(#[from] FromUtf8Error),
	#[error("Unkown enum variant: {0}")]
	UnknownEnumVariant(u16),
}
