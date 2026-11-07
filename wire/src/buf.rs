use crate::{Bufferable, Convertable, DataBuilder, DataReader, ReadError, WriteError};
use rustix::{
	fs::{FileType, MemfdFlags, OFlags, copy_file_range, fcntl_getfl, fstat, memfd_create},
	io::Errno,
};
use std::{
	fs::File,
	io::{self, ErrorKind},
	marker::PhantomData,
	os::{
		fd::{AsFd, BorrowedFd, OwnedFd},
		unix::fs::FileExt,
	},
};
use thiserror::Error;

/// the contents of a [`Buf`] that's just bytes, not an encoded value
pub enum Raw {}

/// a sized readable file sent by fd, for data that won't fit in a message
///
/// the file isn't sealed, so whoever sent it can still rewrite or truncate it. reads use
/// `pread` and never trust the length past what the file actually has, so the worst a
/// peer can do is hand you garbage or a [`BufError::TooShort`]
pub struct Buf<T = Raw> {
	file: File,
	len: u64,
	_t: PhantomData<fn() -> T>,
}
impl<T> Buf<T> {
	/// any fd will do as long as it's a regular file with at least `len` bytes you can read
	pub fn from_fd(fd: impl Into<OwnedFd>, len: u64) -> Result<Self, BufError> {
		let file = File::from(fd.into());
		check(&file, len)?;
		Ok(Self {
			file,
			len,
			_t: PhantomData,
		})
	}
	pub fn len(&self) -> u64 {
		self.len
	}
	pub fn is_empty(&self) -> bool {
		self.len == 0
	}
	/// for when the sender changed the file and only told you the new length, so the same
	/// fd keeps working without sending a new one
	pub fn set_len(&mut self, len: u64) -> Result<(), BufError> {
		check(&self.file, len)?;
		self.len = len;
		Ok(())
	}

	pub fn bytes(&self) -> Result<Vec<u8>, BufError> {
		let mut out = Vec::new();
		self.bytes_into(&mut out)?;
		Ok(out)
	}
	/// [`Buf::bytes`] into an old buffer so rereading doesn't allocate
	pub fn bytes_into(&self, out: &mut Vec<u8>) -> Result<(), BufError> {
		let len = usize::try_from(self.len).map_err(|_| BufError::TooLarge(self.len))?;
		out.clear();
		out.try_reserve_exact(len)
			.map_err(|_| BufError::TooLarge(self.len))?;
		out.resize(len, 0);
		self.file.read_exact_at(out, 0).map_err(eof_is_short)
	}

	/// writes the contents to the start of `out`, in the kernel when it can so the bytes
	/// never pass through this process
	pub fn copy_to(&self, out: &File) -> Result<(), BufError> {
		let (mut src, mut dst) = (0, 0);
		while src < self.len {
			let left = usize::try_from(self.len - src).unwrap_or(usize::MAX);
			match copy_file_range(&self.file, Some(&mut src), out, Some(&mut dst), left) {
				Ok(0) => return Err(BufError::TooShort),
				Ok(_) => {}
				// across filesystems (memfd to disk included) the kernel refuses
				Err(Errno::XDEV | Errno::INVAL | Errno::NOSYS | Errno::OPNOTSUPP) if src == 0 => {
					return Ok(out.write_all_at(&self.bytes()?, 0)?);
				}
				Err(e) => return Err(e.into()),
			}
		}
		Ok(())
	}
}
impl Buf<Raw> {
	pub fn from_bytes(bytes: &[u8]) -> Result<Self, BufError> {
		let len = bytes.len() as u64;
		Ok(Self {
			file: memfd(bytes)?,
			len,
			_t: PhantomData,
		})
	}
}
impl<T: Bufferable> Buf<T> {
	pub fn new(value: &T) -> Result<Self, WriteError> {
		let mut data = DataBuilder::bare();
		value.write(&mut data)?;
		let bytes = data.into_bytes()?;
		Ok(Self {
			file: memfd(&bytes)?,
			len: bytes.len() as u64,
			_t: PhantomData,
		})
	}
	pub fn decode(&self) -> Result<T, ReadError> {
		T::read(&mut DataReader::from_bytes(self.bytes()?))
	}
	/// decodes over `out` with [`Convertable::read_into`], keeping `scratch` around between
	/// calls so a buf that keeps getting updated doesn't allocate once it settles
	pub fn decode_into(&self, out: &mut T, scratch: &mut Vec<u8>) -> Result<(), ReadError> {
		self.bytes_into(scratch)?;
		let mut data = DataReader::from_bytes(std::mem::take(scratch));
		let res = out.read_into(&mut data);
		*scratch = data.into_bytes();
		res
	}
}
impl<T> AsFd for Buf<T> {
	fn as_fd(&self) -> BorrowedFd<'_> {
		self.file.as_fd()
	}
}
impl<T: 'static> Convertable for Buf<T> {
	fn write(&self, data: &mut DataBuilder) -> Result<(), WriteError> {
		data.write_u64(self.len)?;
		data.write_fd(self.file.as_fd())
	}
	fn write_owned(self, data: &mut DataBuilder) -> Result<(), WriteError> {
		data.write_u64(self.len)?;
		data.write_owned_fd(self.file.into())
	}
	fn read(data: &mut DataReader) -> Result<Self, ReadError> {
		let len = data.read_u64()?;
		Ok(Self::from_fd(data.read_fd()?, len)?)
	}
}

fn check(file: &File, len: u64) -> Result<(), BufError> {
	let stat = fstat(file)?;
	if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile {
		return Err(BufError::NotAFile);
	}
	let flags = fcntl_getfl(file)?;
	if flags.contains(OFlags::PATH) || flags.intersection(OFlags::RWMODE) == OFlags::WRONLY {
		return Err(BufError::NotReadable);
	}
	if (stat.st_size as u64) < len {
		return Err(BufError::TooShort);
	}
	Ok(())
}

fn memfd(bytes: &[u8]) -> Result<File, BufError> {
	let file = File::from(memfd_create("gluon-buf", MemfdFlags::CLOEXEC)?);
	file.write_all_at(bytes, 0)?;
	Ok(file)
}

fn eof_is_short(e: io::Error) -> BufError {
	match e.kind() {
		ErrorKind::UnexpectedEof => BufError::TooShort,
		_ => e.into(),
	}
}

#[derive(Debug, Error)]
pub enum BufError {
	#[error("Buf fd is not a regular file")]
	NotAFile,
	#[error("Buf fd is not open for reading")]
	NotReadable,
	#[error("Buf file is shorter than its length")]
	TooShort,
	#[error("Buf of {0} bytes is too large to read into memory")]
	TooLarge(u64),
	#[error("Buf io failed: {0}")]
	Io(#[from] io::Error),
}
impl From<Errno> for BufError {
	fn from(e: Errno) -> Self {
		Self::Io(e.into())
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::FdVec;
	use std::fs::OpenOptions;

	fn send<T: 'static>(buf: &Buf<T>) -> Result<Buf<T>, ReadError> {
		let mut data = DataBuilder::new();
		data.write_u64(buf.len()).unwrap();
		let fds: FdVec = [buf.as_fd().try_clone_to_owned().unwrap()]
			.into_iter()
			.collect();
		let msg = data.finish(8);
		let (_, mut reader) = DataReader::from_wire(msg.data(), fds)?;
		Buf::read(&mut reader)
	}

	fn tmp(name: &str) -> (std::path::PathBuf, File) {
		let path = std::env::temp_dir().join(format!("gluon-buf-{}-{name}", std::process::id()));
		let file = OpenOptions::new()
			.read(true)
			.write(true)
			.create(true)
			.truncate(true)
			.open(&path)
			.unwrap();
		(path, file)
	}

	#[test]
	fn round_trip() {
		let v = vec![vec![1.0f32, 2.0], vec![], vec![3.5; 4000]];
		let buf = send(&Buf::new(&v).unwrap()).unwrap();
		assert!(buf.len() > crate::MAX_MESSAGE_SIZE as u64);
		assert_eq!(buf.decode().unwrap(), v);
	}

	#[test]
	fn raw_round_trip() {
		let buf = send(&Buf::from_bytes(b"hello").unwrap()).unwrap();
		assert_eq!(buf.bytes().unwrap(), b"hello");
	}

	#[test]
	fn decode_into_reuses_scratch() {
		let buf = Buf::new(&vec![String::from("hi"); 64]).unwrap();
		let (mut out, mut scratch) = (Vec::new(), Vec::new());
		buf.decode_into(&mut out, &mut scratch).unwrap();
		let ptr = scratch.as_ptr();
		buf.decode_into(&mut out, &mut scratch).unwrap();
		assert_eq!(scratch.as_ptr(), ptr);
		assert_eq!(out, vec![String::from("hi"); 64]);
	}

	#[test]
	fn pipe_is_not_a_file() {
		let (r, _w) = std::io::pipe().unwrap();
		assert!(matches!(Buf::<Raw>::from_fd(r, 0), Err(BufError::NotAFile)));
	}

	#[test]
	fn write_only_is_not_readable() {
		let (path, _) = tmp("wronly");
		let file = OpenOptions::new().write(true).open(&path).unwrap();
		let _ = std::fs::remove_file(&path);
		assert!(matches!(
			Buf::<Raw>::from_fd(file, 0),
			Err(BufError::NotReadable)
		));
	}

	#[test]
	fn short_file_is_refused() {
		let buf = Buf::from_bytes(b"abc").unwrap();
		let fd = buf.as_fd().try_clone_to_owned().unwrap();
		assert!(matches!(
			Buf::<Raw>::from_fd(fd, 4),
			Err(BufError::TooShort)
		));
	}

	#[test]
	fn truncated_after_receive() {
		let sent = Buf::from_bytes(&[7; 100]).unwrap();
		let got = send(&sent).unwrap();
		sent.file.set_len(10).unwrap();
		assert!(matches!(got.bytes(), Err(BufError::TooShort)));
	}

	#[test]
	fn set_len_follows_the_file() {
		let sent = Buf::from_bytes(b"abc").unwrap();
		let mut got = send(&sent).unwrap();
		sent.file.write_all_at(b"defg", 3).unwrap();
		got.set_len(7).unwrap();
		assert_eq!(got.bytes().unwrap(), b"abcdefg");
		assert!(matches!(got.set_len(8), Err(BufError::TooShort)));
	}

	#[test]
	fn copy_to_disk() {
		let (path, file) = tmp("copy");
		let buf = Buf::from_bytes(&[42; 20000]).unwrap();
		buf.copy_to(&file).unwrap();
		let _ = std::fs::remove_file(&path);
		let mut out = vec![0; 20000];
		file.read_exact_at(&mut out, 0).unwrap();
		assert_eq!(out, vec![42; 20000]);
	}
}
