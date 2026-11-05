use crate::{Convertable, ReadError, Ref, WriteError};
use std::{
	collections::{HashMap, HashSet},
	hash::Hash,
	os::fd::{AsFd, OwnedFd},
};

impl<T: Convertable> Convertable for Box<T> {
	fn write(&self, data: &mut crate::DataBuilder) -> Result<(), WriteError> {
		(**self).write(data)
	}

	fn write_owned(self, data: &mut crate::DataBuilder) -> Result<(), WriteError> {
		(*self).write_owned(data)
	}

	fn read(data: &mut crate::DataReader) -> Result<Self, ReadError> {
		T::read(data).map(Box::new)
	}
}

impl<T: Convertable, E: Convertable> Convertable for Result<T, E> {
	fn write(&self, data: &mut crate::DataBuilder) -> Result<(), WriteError> {
		data.write_bool(self.is_ok())?;
		match self {
			Ok(v) => v.write(data)?,
			Err(e) => e.write(data)?,
		}
		Ok(())
	}

	fn write_owned(self, data: &mut crate::DataBuilder) -> Result<(), WriteError> {
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
}
impl<K: Hash + Eq + Convertable, V: Convertable> Convertable for HashMap<K, V> {
	fn write(&self, data: &mut crate::DataBuilder) -> Result<(), WriteError> {
		data.write_u32(self.len().try_into().map_err(|_| WriteError::ListToLong)?)?;
		for (k, v) in self.iter() {
			k.write(data)?;
			v.write(data)?;
		}
		Ok(())
	}

	fn write_owned(self, data: &mut crate::DataBuilder) -> Result<(), WriteError> {
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
}
impl<T: Hash + Eq + Convertable> Convertable for HashSet<T> {
	fn write(&self, data: &mut crate::DataBuilder) -> Result<(), WriteError> {
		data.write_u32(self.len().try_into().map_err(|_| WriteError::ListToLong)?)?;
		for v in self.iter() {
			v.write(data)?;
		}
		Ok(())
	}

	fn write_owned(self, data: &mut crate::DataBuilder) -> Result<(), WriteError> {
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
}

impl<T: Convertable> Convertable for Vec<T> {
	fn write(&self, data: &mut crate::DataBuilder) -> Result<(), WriteError> {
		data.write_u32(self.len().try_into().map_err(|_| WriteError::ListToLong)?)?;
		for v in self.iter() {
			v.write(data)?;
		}
		Ok(())
	}

	fn write_owned(self, data: &mut crate::DataBuilder) -> Result<(), WriteError> {
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
}
impl<T: Convertable> Convertable for Option<T> {
	fn write(&self, data: &mut crate::DataBuilder) -> Result<(), WriteError> {
		data.write_bool(self.is_some())?;
		if let Some(v) = self {
			v.write(data)?;
		}
		Ok(())
	}

	fn write_owned(self, data: &mut crate::DataBuilder) -> Result<(), WriteError> {
		data.write_bool(self.is_some())?;
		if let Some(v) = self {
			v.write_owned(data)?;
		}
		Ok(())
	}

	fn read(data: &mut crate::DataReader) -> Result<Self, ReadError> {
		data.read_bool()?.then(|| T::read(data)).transpose()
	}
}

impl Convertable for Ref {
	fn write(&self, data: &mut crate::DataBuilder) -> Result<(), WriteError> {
		data.write_ref(self)
	}

	fn read(data: &mut crate::DataReader) -> Result<Self, ReadError> {
		data.read_ref()
	}

	fn write_owned(self, data: &mut crate::DataBuilder) -> Result<(), WriteError> {
		data.write_ref(&self)
	}
}
impl Convertable for OwnedFd {
	fn write(&self, data: &mut crate::DataBuilder) -> Result<(), WriteError> {
		data.write_fd(self.as_fd())
	}

	fn read(data: &mut crate::DataReader) -> Result<Self, ReadError> {
		data.read_fd()
	}

	fn write_owned(self, data: &mut crate::DataBuilder) -> Result<(), WriteError> {
		data.write_owned_fd(self)
	}
}
impl Convertable for String {
	fn write(&self, data: &mut crate::DataBuilder) -> Result<(), WriteError> {
		data.write_str(self)
	}

	fn read(data: &mut crate::DataReader) -> Result<Self, ReadError> {
		data.read_string()
	}

	fn write_owned(self, data: &mut crate::DataBuilder) -> Result<(), WriteError> {
		data.write_str(&self)
	}
}

impl Convertable for bool {
	fn write(&self, data: &mut crate::DataBuilder) -> Result<(), WriteError> {
		data.write_bool(*self)
	}
	fn read(data: &mut crate::DataReader) -> Result<Self, ReadError> {
		data.read_bool()
	}
	fn write_owned(self, data: &mut crate::DataBuilder) -> Result<(), WriteError> {
		data.write_bool(self)
	}
}
impl Convertable for u64 {
	fn write(&self, data: &mut crate::DataBuilder) -> Result<(), WriteError> {
		data.write_u64(*self)
	}
	fn read(data: &mut crate::DataReader) -> Result<Self, ReadError> {
		data.read_u64()
	}
	fn write_owned(self, data: &mut crate::DataBuilder) -> Result<(), WriteError> {
		data.write_u64(self)
	}
}
impl Convertable for i64 {
	fn write(&self, data: &mut crate::DataBuilder) -> Result<(), WriteError> {
		data.write_i64(*self)
	}
	fn read(data: &mut crate::DataReader) -> Result<Self, ReadError> {
		data.read_i64()
	}
	fn write_owned(self, data: &mut crate::DataBuilder) -> Result<(), WriteError> {
		data.write_i64(self)
	}
}
impl Convertable for f64 {
	fn write(&self, data: &mut crate::DataBuilder) -> Result<(), WriteError> {
		data.write_f64(*self)
	}
	fn read(data: &mut crate::DataReader) -> Result<Self, ReadError> {
		data.read_f64()
	}
	fn write_owned(self, data: &mut crate::DataBuilder) -> Result<(), WriteError> {
		data.write_f64(self)
	}
}
impl Convertable for u32 {
	fn write(&self, data: &mut crate::DataBuilder) -> Result<(), WriteError> {
		data.write_u32(*self)
	}
	fn read(data: &mut crate::DataReader) -> Result<Self, ReadError> {
		data.read_u32()
	}
	fn write_owned(self, data: &mut crate::DataBuilder) -> Result<(), WriteError> {
		data.write_u32(self)
	}
}
impl Convertable for i32 {
	fn write(&self, data: &mut crate::DataBuilder) -> Result<(), WriteError> {
		data.write_i32(*self)
	}
	fn read(data: &mut crate::DataReader) -> Result<Self, ReadError> {
		data.read_i32()
	}
	fn write_owned(self, data: &mut crate::DataBuilder) -> Result<(), WriteError> {
		data.write_i32(self)
	}
}
impl Convertable for f32 {
	fn write(&self, data: &mut crate::DataBuilder) -> Result<(), WriteError> {
		data.write_f32(*self)
	}
	fn read(data: &mut crate::DataReader) -> Result<Self, ReadError> {
		data.read_f32()
	}
	fn write_owned(self, data: &mut crate::DataBuilder) -> Result<(), WriteError> {
		data.write_f32(self)
	}
}
impl Convertable for u16 {
	fn write(&self, data: &mut crate::DataBuilder) -> Result<(), WriteError> {
		data.write_u16(*self)
	}
	fn read(data: &mut crate::DataReader) -> Result<Self, ReadError> {
		data.read_u16()
	}
	fn write_owned(self, data: &mut crate::DataBuilder) -> Result<(), WriteError> {
		data.write_u16(self)
	}
}
impl Convertable for i16 {
	fn write(&self, data: &mut crate::DataBuilder) -> Result<(), WriteError> {
		data.write_i16(*self)
	}
	fn read(data: &mut crate::DataReader) -> Result<Self, ReadError> {
		data.read_i16()
	}
	fn write_owned(self, data: &mut crate::DataBuilder) -> Result<(), WriteError> {
		data.write_i16(self)
	}
}
impl Convertable for u8 {
	fn write(&self, data: &mut crate::DataBuilder) -> Result<(), WriteError> {
		data.write_u8(*self)
	}
	fn read(data: &mut crate::DataReader) -> Result<Self, ReadError> {
		data.read_u8()
	}
	fn write_owned(self, data: &mut crate::DataBuilder) -> Result<(), WriteError> {
		data.write_u8(self)
	}
}
impl Convertable for i8 {
	fn write(&self, data: &mut crate::DataBuilder) -> Result<(), WriteError> {
		data.write_i8(*self)
	}
	fn read(data: &mut crate::DataReader) -> Result<Self, ReadError> {
		data.read_i8()
	}
	fn write_owned(self, data: &mut crate::DataBuilder) -> Result<(), WriteError> {
		data.write_i8(self)
	}
}
