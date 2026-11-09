use crate::{Bufferable, Convertable, ReadError, Ref, Sink, WriteError};
use std::{
	collections::{HashMap, HashSet},
	hash::Hash,
	os::fd::{AsFd, OwnedFd},
};

impl<T: Convertable> Convertable for Box<T> {
	fn write(&self, data: &mut impl Sink) -> Result<(), WriteError> {
		(**self).write(data)
	}

	fn write_owned(self, data: &mut impl Sink) -> Result<(), WriteError> {
		(*self).write_owned(data)
	}

	fn read(data: &mut crate::DataReader) -> Result<Self, ReadError> {
		T::read(data).map(Box::new)
	}

	fn read_into(&mut self, data: &mut crate::DataReader) -> Result<(), ReadError> {
		(**self).read_into(data)
	}

	fn encoded_len(&self) -> usize {
		(**self).encoded_len()
	}
}

impl<T: Convertable, E: Convertable> Convertable for Result<T, E> {
	fn write(&self, data: &mut impl Sink) -> Result<(), WriteError> {
		data.write_bool(self.is_ok())?;
		match self {
			Ok(v) => v.write(data)?,
			Err(e) => e.write(data)?,
		}
		Ok(())
	}

	fn write_owned(self, data: &mut impl Sink) -> Result<(), WriteError> {
		data.write_bool(self.is_ok())?;
		match self {
			Ok(v) => v.write_owned(data)?,
			Err(e) => e.write_owned(data)?,
		}
		Ok(())
	}

	fn read(data: &mut crate::DataReader) -> Result<Self, ReadError> {
		let is_ok = data.read_bool()?;
		let v = match is_ok {
			true => Ok(T::read(data)?),
			false => Err(E::read(data)?),
		};
		Ok(v)
	}

	fn read_into(&mut self, data: &mut crate::DataReader) -> Result<(), ReadError> {
		match (data.read_bool()?, self) {
			(true, Ok(v)) => return v.read_into(data),
			(false, Err(e)) => return e.read_into(data),
			(true, s) => *s = Ok(T::read(data)?),
			(false, s) => *s = Err(E::read(data)?),
		}
		Ok(())
	}

	fn encoded_len(&self) -> usize {
		1 + match self {
			Ok(v) => v.encoded_len(),
			Err(e) => e.encoded_len(),
		}
	}
}
impl<K: Hash + Eq + Convertable, V: Convertable> Convertable for HashMap<K, V> {
	fn write(&self, data: &mut impl Sink) -> Result<(), WriteError> {
		data.write_u32(self.len().try_into().map_err(|_| WriteError::ListToLong)?)?;
		for (k, v) in self.iter() {
			k.write(data)?;
			v.write(data)?;
		}
		Ok(())
	}

	fn write_owned(self, data: &mut impl Sink) -> Result<(), WriteError> {
		data.write_u32(self.len().try_into().map_err(|_| WriteError::ListToLong)?)?;
		for (k, v) in self.into_iter() {
			k.write_owned(data)?;
			v.write_owned(data)?;
		}
		Ok(())
	}

	fn read(data: &mut crate::DataReader) -> Result<Self, ReadError> {
		let len = data.read_u32()?;
		let mut out = Self::with_capacity(data.capacity_for(len));
		for _ in 0..len {
			out.insert(K::read(data)?, V::read(data)?);
		}
		Ok(out)
	}

	fn read_into(&mut self, data: &mut crate::DataReader) -> Result<(), ReadError> {
		let len = data.read_u32()?;
		self.clear();
		for _ in 0..len {
			self.insert(K::read(data)?, V::read(data)?);
		}
		Ok(())
	}

	fn encoded_len(&self) -> usize {
		4 + self
			.iter()
			.map(|(k, v)| k.encoded_len() + v.encoded_len())
			.sum::<usize>()
	}
}
impl<T: Hash + Eq + Convertable> Convertable for HashSet<T> {
	fn write(&self, data: &mut impl Sink) -> Result<(), WriteError> {
		data.write_u32(self.len().try_into().map_err(|_| WriteError::ListToLong)?)?;
		for v in self.iter() {
			v.write(data)?;
		}
		Ok(())
	}

	fn write_owned(self, data: &mut impl Sink) -> Result<(), WriteError> {
		data.write_u32(self.len().try_into().map_err(|_| WriteError::ListToLong)?)?;
		for v in self.into_iter() {
			v.write_owned(data)?;
		}
		Ok(())
	}

	fn read(data: &mut crate::DataReader) -> Result<Self, ReadError> {
		let len = data.read_u32()?;
		let mut out = Self::with_capacity(data.capacity_for(len));
		for _ in 0..len {
			out.insert(T::read(data)?);
		}
		Ok(out)
	}

	fn read_into(&mut self, data: &mut crate::DataReader) -> Result<(), ReadError> {
		let len = data.read_u32()?;
		self.clear();
		for _ in 0..len {
			self.insert(T::read(data)?);
		}
		Ok(())
	}

	fn encoded_len(&self) -> usize {
		4 + self.iter().map(T::encoded_len).sum::<usize>()
	}
}

impl<T: Convertable> Convertable for Vec<T> {
	fn write(&self, data: &mut impl Sink) -> Result<(), WriteError> {
		data.write_u32(self.len().try_into().map_err(|_| WriteError::ListToLong)?)?;
		for v in self.iter() {
			v.write(data)?;
		}
		Ok(())
	}

	fn write_owned(self, data: &mut impl Sink) -> Result<(), WriteError> {
		data.write_u32(self.len().try_into().map_err(|_| WriteError::ListToLong)?)?;
		for v in self.into_iter() {
			v.write_owned(data)?;
		}
		Ok(())
	}

	fn read(data: &mut crate::DataReader) -> Result<Self, ReadError> {
		let len = data.read_u32()?;
		let mut out = Vec::with_capacity(data.capacity_for(len));
		for _ in 0..len {
			out.push(T::read(data)?);
		}
		Ok(out)
	}

	fn read_into(&mut self, data: &mut crate::DataReader) -> Result<(), ReadError> {
		let len = data.read_u32()? as usize;
		self.truncate(len);
		for v in self.iter_mut() {
			v.read_into(data)?;
		}
		for _ in self.len()..len {
			self.push(T::read(data)?);
		}
		Ok(())
	}

	fn encoded_len(&self) -> usize {
		4 + self.iter().map(T::encoded_len).sum::<usize>()
	}
}
/// no length on the wire, the schema already says how many
impl<T: Convertable, const N: usize> Convertable for [T; N] {
	fn write(&self, data: &mut impl Sink) -> Result<(), WriteError> {
		for v in self {
			v.write(data)?;
		}
		Ok(())
	}

	fn write_owned(self, data: &mut impl Sink) -> Result<(), WriteError> {
		for v in self {
			v.write_owned(data)?;
		}
		Ok(())
	}

	fn read(data: &mut crate::DataReader) -> Result<Self, ReadError> {
		let mut out = Vec::with_capacity(N);
		for _ in 0..N {
			out.push(T::read(data)?);
		}
		Ok(out.try_into().unwrap_or_else(|_| unreachable!()))
	}

	fn read_into(&mut self, data: &mut crate::DataReader) -> Result<(), ReadError> {
		for v in self {
			v.read_into(data)?;
		}
		Ok(())
	}

	fn encoded_len(&self) -> usize {
		self.iter().map(T::encoded_len).sum()
	}
}
impl<T: Convertable> Convertable for Option<T> {
	fn write(&self, data: &mut impl Sink) -> Result<(), WriteError> {
		data.write_bool(self.is_some())?;
		if let Some(v) = self {
			v.write(data)?;
		}
		Ok(())
	}

	fn write_owned(self, data: &mut impl Sink) -> Result<(), WriteError> {
		data.write_bool(self.is_some())?;
		if let Some(v) = self {
			v.write_owned(data)?;
		}
		Ok(())
	}

	fn read(data: &mut crate::DataReader) -> Result<Self, ReadError> {
		data.read_bool()?.then(|| T::read(data)).transpose()
	}

	fn read_into(&mut self, data: &mut crate::DataReader) -> Result<(), ReadError> {
		match (data.read_bool()?, self) {
			(true, Some(v)) => return v.read_into(data),
			(true, s) => *s = Some(T::read(data)?),
			(false, s) => *s = None,
		}
		Ok(())
	}

	fn encoded_len(&self) -> usize {
		1 + self.as_ref().map_or(0, T::encoded_len)
	}
}

impl Convertable for Ref {
	fn write(&self, data: &mut impl Sink) -> Result<(), WriteError> {
		data.write_ref(self)
	}

	fn read(data: &mut crate::DataReader) -> Result<Self, ReadError> {
		data.read_ref()
	}

	fn write_owned(self, data: &mut impl Sink) -> Result<(), WriteError> {
		data.write_ref(&self)
	}

	fn encoded_len(&self) -> usize {
		0
	}
}
impl Convertable for OwnedFd {
	fn write(&self, data: &mut impl Sink) -> Result<(), WriteError> {
		data.write_fd(self.as_fd())
	}

	fn read(data: &mut crate::DataReader) -> Result<Self, ReadError> {
		data.read_fd()
	}

	fn write_owned(self, data: &mut impl Sink) -> Result<(), WriteError> {
		data.write_owned_fd(self)
	}

	fn encoded_len(&self) -> usize {
		0
	}
}
impl Convertable for String {
	fn write(&self, data: &mut impl Sink) -> Result<(), WriteError> {
		data.write_str(self)
	}

	fn read(data: &mut crate::DataReader) -> Result<Self, ReadError> {
		data.read_string()
	}

	fn read_into(&mut self, data: &mut crate::DataReader) -> Result<(), ReadError> {
		data.read_string_into(self)
	}

	fn write_owned(self, data: &mut impl Sink) -> Result<(), WriteError> {
		data.write_str(&self)
	}

	fn encoded_len(&self) -> usize {
		4 + self.len()
	}
}

impl Convertable for bool {
	fn write(&self, data: &mut impl Sink) -> Result<(), WriteError> {
		data.write_bool(*self)
	}
	fn read(data: &mut crate::DataReader) -> Result<Self, ReadError> {
		data.read_bool()
	}
	fn write_owned(self, data: &mut impl Sink) -> Result<(), WriteError> {
		data.write_bool(self)
	}

	fn encoded_len(&self) -> usize {
		size_of::<Self>()
	}
}
impl Convertable for u64 {
	fn write(&self, data: &mut impl Sink) -> Result<(), WriteError> {
		data.write_u64(*self)
	}
	fn read(data: &mut crate::DataReader) -> Result<Self, ReadError> {
		data.read_u64()
	}
	fn write_owned(self, data: &mut impl Sink) -> Result<(), WriteError> {
		data.write_u64(self)
	}

	fn encoded_len(&self) -> usize {
		size_of::<Self>()
	}
}
impl Convertable for i64 {
	fn write(&self, data: &mut impl Sink) -> Result<(), WriteError> {
		data.write_i64(*self)
	}
	fn read(data: &mut crate::DataReader) -> Result<Self, ReadError> {
		data.read_i64()
	}
	fn write_owned(self, data: &mut impl Sink) -> Result<(), WriteError> {
		data.write_i64(self)
	}

	fn encoded_len(&self) -> usize {
		size_of::<Self>()
	}
}
impl Convertable for f64 {
	fn write(&self, data: &mut impl Sink) -> Result<(), WriteError> {
		data.write_f64(*self)
	}
	fn read(data: &mut crate::DataReader) -> Result<Self, ReadError> {
		data.read_f64()
	}
	fn write_owned(self, data: &mut impl Sink) -> Result<(), WriteError> {
		data.write_f64(self)
	}

	fn encoded_len(&self) -> usize {
		size_of::<Self>()
	}
}
impl Convertable for u32 {
	fn write(&self, data: &mut impl Sink) -> Result<(), WriteError> {
		data.write_u32(*self)
	}
	fn read(data: &mut crate::DataReader) -> Result<Self, ReadError> {
		data.read_u32()
	}
	fn write_owned(self, data: &mut impl Sink) -> Result<(), WriteError> {
		data.write_u32(self)
	}

	fn encoded_len(&self) -> usize {
		size_of::<Self>()
	}
}
impl Convertable for i32 {
	fn write(&self, data: &mut impl Sink) -> Result<(), WriteError> {
		data.write_i32(*self)
	}
	fn read(data: &mut crate::DataReader) -> Result<Self, ReadError> {
		data.read_i32()
	}
	fn write_owned(self, data: &mut impl Sink) -> Result<(), WriteError> {
		data.write_i32(self)
	}

	fn encoded_len(&self) -> usize {
		size_of::<Self>()
	}
}
impl Convertable for f32 {
	fn write(&self, data: &mut impl Sink) -> Result<(), WriteError> {
		data.write_f32(*self)
	}
	fn read(data: &mut crate::DataReader) -> Result<Self, ReadError> {
		data.read_f32()
	}
	fn write_owned(self, data: &mut impl Sink) -> Result<(), WriteError> {
		data.write_f32(self)
	}

	fn encoded_len(&self) -> usize {
		size_of::<Self>()
	}
}
impl Convertable for u16 {
	fn write(&self, data: &mut impl Sink) -> Result<(), WriteError> {
		data.write_u16(*self)
	}
	fn read(data: &mut crate::DataReader) -> Result<Self, ReadError> {
		data.read_u16()
	}
	fn write_owned(self, data: &mut impl Sink) -> Result<(), WriteError> {
		data.write_u16(self)
	}

	fn encoded_len(&self) -> usize {
		size_of::<Self>()
	}
}
impl Convertable for i16 {
	fn write(&self, data: &mut impl Sink) -> Result<(), WriteError> {
		data.write_i16(*self)
	}
	fn read(data: &mut crate::DataReader) -> Result<Self, ReadError> {
		data.read_i16()
	}
	fn write_owned(self, data: &mut impl Sink) -> Result<(), WriteError> {
		data.write_i16(self)
	}

	fn encoded_len(&self) -> usize {
		size_of::<Self>()
	}
}
impl Convertable for u8 {
	fn write(&self, data: &mut impl Sink) -> Result<(), WriteError> {
		data.write_u8(*self)
	}
	fn read(data: &mut crate::DataReader) -> Result<Self, ReadError> {
		data.read_u8()
	}
	fn write_owned(self, data: &mut impl Sink) -> Result<(), WriteError> {
		data.write_u8(self)
	}

	fn encoded_len(&self) -> usize {
		size_of::<Self>()
	}
}
impl Convertable for i8 {
	fn write(&self, data: &mut impl Sink) -> Result<(), WriteError> {
		data.write_i8(*self)
	}
	fn read(data: &mut crate::DataReader) -> Result<Self, ReadError> {
		data.read_i8()
	}
	fn write_owned(self, data: &mut impl Sink) -> Result<(), WriteError> {
		data.write_i8(self)
	}

	fn encoded_len(&self) -> usize {
		size_of::<Self>()
	}
}

impl<T: Bufferable> Bufferable for Box<T> {}
impl<T: Bufferable, E: Bufferable> Bufferable for Result<T, E> {}
impl<K: Hash + Eq + Bufferable, V: Bufferable> Bufferable for HashMap<K, V> {}
impl<T: Hash + Eq + Bufferable> Bufferable for HashSet<T> {}
impl<T: Bufferable> Bufferable for Vec<T> {}
impl<T: Bufferable, const N: usize> Bufferable for [T; N] {}
impl<T: Bufferable> Bufferable for Option<T> {}
impl Bufferable for String {}
impl Bufferable for bool {}
impl Bufferable for u64 {}
impl Bufferable for i64 {}
impl Bufferable for f64 {}
impl Bufferable for u32 {}
impl Bufferable for i32 {}
impl Bufferable for f32 {}
impl Bufferable for u16 {}
impl Bufferable for i16 {}
impl Bufferable for u8 {}
impl Bufferable for i8 {}
