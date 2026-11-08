use gluon_ipc_codegen::{Derives, TypeProxy, helpers::gen_multiple_modules};
use std::path::Path;

fn main() {
	println!("cargo:rerun-if-changed=../codegen-rust/src/lib.rs");
	gen_multiple_modules(
		&[
			(
				"types",
				Path::new("./gluon/org.stardustxr.gluon.Types.gluon"),
			),
			("test", Path::new("./gluon/org.stardustxr.gluon.Test.gluon")),
		],
		&[],
		Derives::all() & !Derives::SERDE,
		&[
			TypeProxy {
				protocol_type_name: "test::TestEnum".into(),
				rust_type: "crate::MyTestEnum".into(),
				derives: Derives::empty(),
			},
			TypeProxy {
				protocol_type_name: "test::Color".into(),
				rust_type: "crate::MyColor".into(),
				derives: Derives::COPY
					| Derives::CLONE
					| Derives::HASH | Derives::PARTIAL_EQ
					| Derives::EQ,
			},
			TypeProxy {
				protocol_type_name: "types::Vec3".into(),
				rust_type: "crate::MyVec3".into(),
				derives: Derives::COPY | Derives::CLONE | Derives::PARTIAL_EQ,
			},
		],
		true,
		"./src/protocol",
	);
}
