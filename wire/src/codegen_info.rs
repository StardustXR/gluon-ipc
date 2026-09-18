bitflags::bitflags! {
	#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
	pub struct Derives: u32 {
		const COPY        = 1 << 0;
		const CLONE       = 1 << 1;
		const HASH        = 1 << 2;
		const PARTIAL_EQ  = 1 << 3;
		const EQ          = 1 << 4;
		const PARTIAL_ORD = 1 << 5;
		const ORD         = 1 << 6;
		const DEFAULT     = 1 << 7;
		const SERDE_SER   = 1 << 8;
		const SERDE_DE    = 1 << 9;

		/// All serde derives
		const SERDE = Self::SERDE_SER.bits() | Self::SERDE_DE.bits();
		/// All standard derives that integer types support
		const INTEGERS = Self::COPY.bits() | Self::CLONE.bits() | Self::HASH.bits()
			| Self::PARTIAL_EQ.bits() | Self::EQ.bits()
			| Self::PARTIAL_ORD.bits() | Self::ORD.bits()
			| Self::DEFAULT.bits() | Self::SERDE_SER.bits() | Self::SERDE_DE.bits();
		/// All standard derives that float types support (no Hash, Eq, or Ord)
		const FLOATS = Self::COPY.bits() | Self::CLONE.bits()
			| Self::PARTIAL_EQ.bits() | Self::PARTIAL_ORD.bits()
			| Self::DEFAULT.bits() | Self::SERDE_SER.bits() | Self::SERDE_DE.bits();
	}
}

#[derive(Clone, Copy, Debug)]
pub struct ExternalProtocol {
	pub protocol_name: &'static str,
	pub types: &'static [ExternalGluonType],
}
#[derive(Clone, Copy, Debug)]
pub struct ExternalGluonType {
	pub name: &'static str,
	pub proxy: Option<&'static str>,
	pub supported_derives: Derives,
}
