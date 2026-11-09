use crate::protocol::test::TestHandler;
use gluon_ipc::Context;
use std::{
	hash::{DefaultHasher, Hash},
	process,
	sync::atomic::{AtomicI32, Ordering},
};

mod protocol;

/// Proxy type for the wire `TestEnum` — demonstrates method-signature proxying.
pub struct MyTestEnum(protocol::test::proxied::TestEnum);

/// Proxy type for the wire `Color` — demonstrates proxy propagation into struct fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MyColor {
	Red,
	Green,
	Blue,
}

impl From<protocol::test::proxied::TestEnum> for MyTestEnum {
	fn from(v: protocol::test::proxied::TestEnum) -> Self {
		MyTestEnum(v)
	}
}
impl From<MyTestEnum> for protocol::test::proxied::TestEnum {
	fn from(v: MyTestEnum) -> Self {
		v.0
	}
}

/// Proxy type for the wire `types::Vec3` — demonstrates cross-protocol proxying.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MyVec3 {
	pub x: f32,
	pub y: f32,
	pub z: f32,
}

impl From<protocol::types::proxied::Vec3> for MyVec3 {
	fn from(v: protocol::types::proxied::Vec3) -> Self {
		MyVec3 {
			x: v.x,
			y: v.y,
			z: v.z,
		}
	}
}
impl From<MyVec3> for protocol::types::proxied::Vec3 {
	fn from(v: MyVec3) -> Self {
		protocol::types::proxied::Vec3 {
			x: v.x,
			y: v.y,
			z: v.z,
		}
	}
}

impl From<protocol::test::proxied::Color> for MyColor {
	fn from(c: protocol::test::proxied::Color) -> Self {
		match c {
			protocol::test::proxied::Color::Red => MyColor::Red,
			protocol::test::proxied::Color::Green => MyColor::Green,
			protocol::test::proxied::Color::Blue => MyColor::Blue,
		}
	}
}
impl From<MyColor> for protocol::test::proxied::Color {
	fn from(c: MyColor) -> Self {
		match c {
			MyColor::Red => protocol::test::proxied::Color::Red,
			MyColor::Green => protocol::test::proxied::Color::Green,
			MyColor::Blue => protocol::test::proxied::Color::Blue,
		}
	}
}

#[allow(unused)]
#[derive(Debug, Default, gluon_ipc::Handler)]
struct TestHandlerImpl {
	/// pid the last `ping` came from, or 0 if the kernel reported no credentials —
	/// lets a test assert that `Context` really carries `SCM_CREDENTIALS` through
	last_ping_pid: AtomicI32,
}

impl TestHandler for TestHandlerImpl {
	async fn quit(&self, _ctx: Context) {
		process::exit(0);
	}

	async fn ping(&self, _ctx: Context) {
		self.last_ping_pid
			.store(_ctx.sender_pid().unwrap_or(0), Ordering::Relaxed);
		println!("got ping");
		let mut hasher = DefaultHasher::new();
		c"nya~".to_owned().hash(&mut hasher);
	}

	async fn echo(&self, _ctx: Context, input: MyTestEnum) -> MyTestEnum {
		input
	}

	async fn echo_ref(&self, _ctx: Context, input: protocol::test::Test) -> protocol::test::Test {
		input
	}

	async fn echo_untyped_ref(
		&self,
		_ctx: gluon_ipc::Context,
		input: gluon_ipc::Ref,
	) -> gluon_ipc::Ref {
		input
	}

	async fn get_position(&self, _ctx: Context) -> MyVec3 {
		MyVec3 {
			x: 1.0,
			y: 2.0,
			z: 3.0,
		}
	}

	async fn count_shapes(
		&self,
		_ctx: Context,
		shapes: gluon_ipc::Buf<Vec<protocol::test::Shape>>,
	) -> u32 {
		shapes.decode().map_or(0, |v| v.len() as u32)
	}
}

/// A second handler for the same interface, so a test can ask a proxy for the *wrong*
/// handler type and get `None` rather than a compile error — the `HandledBy` bound admits
/// both of these, and only the registry lookup can tell them apart.
#[cfg(test)]
#[derive(Debug, gluon_ipc::Handler)]
struct SecondTestHandler;

#[cfg(test)]
impl TestHandler for SecondTestHandler {
	async fn quit(&self, _ctx: Context) {}
	async fn ping(&self, _ctx: Context) {}
	async fn echo(&self, _ctx: Context, input: MyTestEnum) -> MyTestEnum {
		input
	}
	async fn echo_ref(&self, _ctx: Context, input: protocol::test::Test) -> protocol::test::Test {
		input
	}
	async fn echo_untyped_ref(
		&self,
		_ctx: gluon_ipc::Context,
		input: gluon_ipc::Ref,
	) -> gluon_ipc::Ref {
		input
	}
	async fn get_position(&self, _ctx: Context) -> MyVec3 {
		MyVec3 {
			x: 0.0,
			y: 0.0,
			z: 0.0,
		}
	}
	async fn count_shapes(
		&self,
		_ctx: Context,
		_shapes: gluon_ipc::Buf<Vec<protocol::test::Shape>>,
	) -> u32 {
		0
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use gluon_ipc::{Liveness, RefExt, ToRef};
	use protocol::test::{Test, TestLocal};
	use std::sync::Arc;

	/// Stands up a handler and the [`gluon_ipc::LocalRef`] that reaches it.
	///
	/// The node comes back alongside it because nothing else holds it: drop it and its
	/// socket hangs up, so it has to outlive every call made through the proxy.
	fn test_node() -> (gluon_ipc::Node<TestHandlerImpl>, TestLocal<TestHandlerImpl>) {
		Test::new_node(TestHandlerImpl::default()).unwrap()
	}

	/// A handler already in an `Arc` goes in as-is — that's what the `Into<Arc<H>>` bound
	/// buys over a second `_raw` constructor.
	#[tokio::test]
	async fn a_shared_handler_needs_no_second_constructor() {
		let handler = Arc::new(TestHandlerImpl::default());
		let (_node, local) = Test::new_node(handler.clone()).unwrap();
		local.proxy().ping().await.unwrap();
		assert_eq!(
			handler.last_ping_pid.load(Ordering::Relaxed),
			process::id() as i32
		);
		// the same handler, reached through the `LocalRef` rather than the variable
		assert!(Arc::ptr_eq(local.handler(), &handler));
	}

	/// A service keeps serving with no node held anywhere, which is the whole point of it
	/// — and `H` is inferred from the handler alone here, since it appears nowhere in the
	/// return type.
	#[tokio::test]
	async fn a_service_serves_without_a_node_to_hold() {
		let local = Test::new_service(TestHandlerImpl::default()).unwrap();
		local.proxy().ping().await.unwrap();
		assert!(local.alive());
		// and the handler is still right there, with no node held anywhere
		assert_eq!(
			local.last_ping_pid.load(Ordering::Relaxed),
			process::id() as i32
		);
	}

	/// The bootstrap path: a proxy built from a filesystem path rather than from a ref
	/// somebody handed us, talking to a node published there with [`RefExt::bind`].
	#[tokio::test]
	async fn connect_reaches_a_bound_node() {
		let path = std::env::temp_dir().join(format!("gluon-connect-{}.sock", process::id()));
		// the binding never unlinks, so a run killed before its drop leaves this behind
		let _ = std::fs::remove_file(&path);
		let (_node, bound) = test_node();
		let _binding = bound.proxy().bind(&path).unwrap();

		let proxy = Test::connect(&path).await.unwrap();
		assert_eq!(
			proxy.get_position().await.unwrap(),
			MyVec3 {
				x: 1.0,
				y: 2.0,
				z: 3.0
			}
		);
	}

	// --- LocalRef ---

	/// The point of the whole thing: an object we built reaches its handler with no node
	/// held, no lookup, and no `Option`.
	#[tokio::test]
	async fn local_ref_reaches_its_handler() {
		let (_node, local) = test_node();
		local.proxy().ping().await.unwrap();
		assert_eq!(
			local.handler().last_ping_pid.load(Ordering::Relaxed),
			process::id() as i32
		);
	}

	/// A `LocalRef` drops into a proxy-typed struct field the same as a proxy does.
	#[tokio::test]
	async fn local_ref_fills_a_proxy_typed_field() {
		let (_node, local) = test_node();
		let test_struct = protocol::test::TestStruct {
			string: "nya~".to_string(),
			id: 7,
			binder_ref: local.clone().into(),
			position: MyVec3 {
				x: 1.0,
				y: 2.0,
				z: 3.0,
			},
		};
		assert_eq!(&test_struct.binder_ref, local.proxy());
	}

	/// Cloning a `LocalRef` clones the `LocalRef`, not the `Arc<H>` it derefs to — the
	/// trap `strong_ipc::Node` refuses this deref over. Both halves come along.
	#[tokio::test]
	async fn cloning_a_local_ref_yields_a_local_ref() {
		let (_node, local) = test_node();
		let cloned: TestLocal<TestHandlerImpl> = local.clone();
		assert_eq!(cloned.proxy(), local.proxy());
		assert!(Arc::ptr_eq(cloned.handler(), local.handler()));
	}

	/// A service's `LocalRef` keeps serving with no node held anywhere, and keeps the
	/// handler reachable for as long as it lives.
	#[tokio::test]
	async fn a_service_local_ref_outlives_its_node() {
		let local = Test::new_service(TestHandlerImpl::default()).unwrap();
		assert!(local.alive());
		local.proxy().ping().await.unwrap();
		assert_eq!(
			local.handler().last_ping_pid.load(Ordering::Relaxed),
			process::id() as i32
		);
	}

	/// The recovery path: a ref sent out and handed back is recognised on arrival, and
	/// `local_from_ref` reuses that very ref as the proxy rather than building a second
	/// one beside it.
	#[tokio::test]
	async fn local_from_ref_recovers_a_ref_off_the_wire() {
		let (_node, local) = test_node();
		let returned: Test = local.proxy().echo_ref(local.clone()).await.unwrap();

		let recovered = Test::local_from_ref(returned.to_ref())
			.expect("a ref to our own live node, behind the handler we built");
		let recovered: TestLocal<TestHandlerImpl> = recovered;

		assert!(Arc::ptr_eq(recovered.handler(), local.handler()));
		assert_eq!(recovered.proxy(), local.proxy());
	}

	/// `as_local` is the same answer for a proxy already in hand.
	#[tokio::test]
	async fn as_local_recovers_from_a_proxy() {
		let (_node, local) = test_node();
		let returned: Test = local.proxy().echo_ref(local.clone()).await.unwrap();

		let recovered = returned.as_local::<TestHandlerImpl>().unwrap();
		assert!(Arc::ptr_eq(recovered.handler(), local.handler()));
	}

	/// The `None` paths. A ref from another process isn't reachable from a unit test, but
	/// a dead node and a mismatched handler type both are, and neither is distinguished —
	/// which is the documented contract.
	#[tokio::test]
	async fn local_from_ref_is_none_when_it_cannot_hand_the_handler_over() {
		// wrong `H`: a live node of ours, but a different handler behind this interface
		let (_other_node, other) = Test::new_node(SecondTestHandler).unwrap();
		assert!(other.proxy().as_local::<TestHandlerImpl>().is_none());
		// ...and asking for the handler it actually has still works
		assert!(other.proxy().as_local::<SecondTestHandler>().is_some());

		// dead node: the ref outlives the node it led to
		let (node, local) = test_node();
		let orphan = local.to_ref();
		drop(node);
		local.death_notification().await;
		assert!(Test::local_from_ref::<TestHandlerImpl>(orphan).is_none());
	}

	// --- over a real socket ---

	#[tokio::test]
	async fn value_return_round_trips() {
		let (_node, local) = test_node();
		assert_eq!(
			local.proxy().get_position().await.unwrap(),
			MyVec3 {
				x: 1.0,
				y: 2.0,
				z: 3.0
			}
		);
	}

	#[tokio::test]
	async fn enum_param_and_return_round_trip() {
		let (_node, local) = test_node();
		let echoed = local
			.proxy()
			.echo(MyTestEnum(protocol::test::proxied::TestEnum::EmptyVariant))
			.await
			.unwrap();
		assert!(matches!(
			echoed.0,
			protocol::test::proxied::TestEnum::EmptyVariant
		));
	}

	#[tokio::test]
	async fn buf_param_goes_over_the_socket() {
		let (_node, local) = test_node();
		let shapes = vec![protocol::test::Shape::Circle { radius: 1.0 }; 3000];
		let buf = gluon_ipc::Buf::new(&shapes).unwrap();
		assert!(buf.len() > gluon_ipc::MAX_MESSAGE_SIZE as u64);
		assert_eq!(local.proxy().count_shapes(buf).await.unwrap(), 3000);
	}

	#[test]
	fn buf_fields_round_trip() {
		use gluon_ipc::{Buf, Convertable};
		use protocol::test::{BufTest, Shape, proxied::Color};
		use std::os::fd::AsFd;
		let v = BufTest {
			raw: Buf::from_bytes(b"raw").unwrap(),
			shapes: Buf::new(&vec![Shape::Empty]).unwrap(),
			colors: Buf::new(&vec![Color::Red, Color::Blue]).unwrap(),
			maybe: None,
		};
		let mut b = gluon_ipc::DataBuilder::new();
		v.write(&mut b).unwrap();
		let fds: gluon_ipc::FdVec = [v.raw.as_fd(), v.shapes.as_fd(), v.colors.as_fd()]
			.into_iter()
			.map(|f| f.try_clone_to_owned().unwrap())
			.collect();
		let (_, mut r) = gluon_ipc::DataReader::from_wire(b.finish(0).data(), fds).unwrap();
		let got = BufTest::read(&mut r).unwrap();
		assert_eq!(got.raw.bytes().unwrap(), b"raw");
		assert_eq!(got.shapes.decode().unwrap(), vec![Shape::Empty]);
		assert_eq!(got.colors.decode().unwrap(), vec![Color::Red, Color::Blue]);
		assert!(got.maybe.is_none());
	}

	/// A ref sent out and echoed back arrives as a descriptor for the same socket, and
	/// strong-ipc's registry is what makes that compare equal to the one we sent.
	#[tokio::test]
	async fn ref_identity_survives_a_round_trip() {
		let (_node, local) = test_node();
		let (_other_node, other) = test_node();

		// `&impl ToRef` takes the `LocalRef` directly — no unwrapping to the proxy
		let sent = other.to_ref();
		let returned = local.proxy().echo_untyped_ref(&other).await.unwrap();
		assert_eq!(returned, sent);
	}

	/// The typed variant of the same trip, through the proxy type rather than a bare ref.
	#[tokio::test]
	async fn typed_ref_round_trips() {
		let (_node, local) = test_node();
		// `impl Into<Test>` takes the `LocalRef` too, through the generated `From` impl
		let returned = local.proxy().echo_ref(local.clone()).await.unwrap();
		assert_eq!(&returned, local.proxy());
	}

	/// `Context` is filled from `SO_PASSCRED`; over a socketpair to ourselves that has
	/// to come back as this process.
	#[tokio::test]
	async fn context_carries_peer_credentials() {
		let (_node, local) = test_node();
		local.proxy().ping().await.unwrap();
		// read straight off the handler through the `Deref`, no `node.handler()` needed
		assert_eq!(
			local.last_ping_pid.load(Ordering::Relaxed),
			process::id() as i32
		);
	}

	/// Dropping the node hangs its socket up, so the proxy's ref goes dead and sends to
	/// it report a gone peer rather than hanging.
	#[tokio::test]
	async fn dropping_the_node_kills_the_ref() {
		let (node, local) = test_node();
		assert!(local.alive());
		drop(node);
		local.death_notification().await;
		assert!(!local.alive());
	}

	// Palette.primary/secondary are MyColor (proxy), not the wire Color — verified by type.
	#[test]
	fn palette_fields_use_proxy_type() {
		let palette = protocol::test::Palette {
			primary: MyColor::Red,
			secondary: MyColor::Blue,
		};
		assert_eq!(palette.primary, MyColor::Red);
		assert_eq!(palette.secondary, MyColor::Blue);
	}

	#[test]
	fn my_color_from_wire_round_trips() {
		use protocol::test::proxied::Color;
		for (wire, expected) in [
			(Color::Red, MyColor::Red),
			(Color::Green, MyColor::Green),
			(Color::Blue, MyColor::Blue),
		] {
			let proxy = MyColor::from(wire);
			assert_eq!(proxy, expected);
			let back = Color::from(proxy);
			assert_eq!(MyColor::from(back), expected);
		}
	}

	#[test]
	fn option_proxy_field_type() {
		// MaybeColor.color should be Option<MyColor>, not Option<wire Color>
		let mc = protocol::test::MaybeColor {
			color: Some(MyColor::Green),
		};
		assert_eq!(mc.color, Some(MyColor::Green));
		let none = protocol::test::MaybeColor { color: None };
		assert_eq!(none.color, None);
	}

	#[test]
	fn cross_protocol_vec3_round_trips() {
		use protocol::types::proxied::Vec3;
		let wire = Vec3 {
			x: 1.0,
			y: 2.0,
			z: 3.0,
		};
		let proxy = MyVec3::from(wire);
		assert_eq!(
			proxy,
			MyVec3 {
				x: 1.0,
				y: 2.0,
				z: 3.0
			}
		);
		let back = Vec3::from(proxy);
		assert_eq!(back.x, 1.0);
		assert_eq!(back.y, 2.0);
		assert_eq!(back.z, 3.0);
	}

	#[test]
	fn palette_copy_semantics() {
		// Palette derives Copy because MyColor declares Derives::COPY.
		let a = protocol::test::Palette {
			primary: MyColor::Green,
			secondary: MyColor::Red,
		};
		let b = a; // copy
		let _ = a; // still valid
		assert_eq!(b.primary, MyColor::Green);
	}

	#[test]
	fn primitives_round_trip() {
		use gluon_ipc::Convertable;
		let v = protocol::test::TypesTest {
			flag: true,
			byte: u8::MAX,
			ushort: u16::MAX,
			uint: u32::MAX,
			ulong: u64::MAX,
			sbyte: i8::MIN,
			short: i16::MIN,
			int: i32::MIN,
			long: i64::MIN,
			float: 0.1,
			double: 0.1,
			string: "hello".into(),
		};
		let mut b = gluon_ipc::DataBuilder::new();
		v.write(&mut b).unwrap();
		let m = b.finish(0);
		let (_, mut r) =
			gluon_ipc::DataReader::from_wire(m.data(), gluon_ipc::FdVec::new()).unwrap();
		assert_eq!(protocol::test::TypesTest::read(&mut r).unwrap(), v);
	}

	fn read_over<T: gluon_ipc::Convertable>(out: &mut T, v: &T) {
		let mut b = gluon_ipc::BareBuilder::new();
		v.write(&mut b).unwrap();
		let mut r = gluon_ipc::DataReader::from_bytes(b.into_bytes());
		out.read_into(&mut r).unwrap();
	}

	#[test]
	fn generated_read_into() {
		use protocol::test::{Palette, Shape};
		let mut shape = Shape::Named { name: "abc".into() };
		let Shape::Named { name } = &shape else {
			unreachable!()
		};
		let ptr = name.as_ptr();
		read_over(&mut shape, &Shape::Named { name: "xyz".into() });
		let Shape::Named { name } = &shape else {
			unreachable!()
		};
		assert_eq!((name.as_str(), name.as_ptr()), ("xyz", ptr));

		read_over(&mut shape, &Shape::Circle { radius: 2.0 });
		assert_eq!(shape, Shape::Circle { radius: 2.0 });
		read_over(&mut shape, &Shape::Empty);
		assert_eq!(shape, Shape::Empty);

		let mut palette = Palette {
			primary: MyColor::Red,
			secondary: MyColor::Red,
		};
		let v = Palette {
			primary: MyColor::Green,
			secondary: MyColor::Blue,
		};
		read_over(&mut palette, &v);
		assert_eq!(palette, v);
	}

	#[test]
	fn generated_arrays() {
		use protocol::test::Arrays;
		let v = Arrays {
			bytes: [1, 2, 3, 4],
			names: ["a".into(), "b".into()],
			colors: [MyColor::Green, MyColor::Blue],
		};
		assert_eq!(gluon_ipc::Buf::new(&v).unwrap().decode().unwrap(), v);
		let mut out = Arrays {
			bytes: [0; 4],
			names: [String::new(), String::new()],
			colors: [MyColor::Red; 2],
		};
		read_over(&mut out, &v);
		assert_eq!(out, v);
	}

	fn assert_len<T: gluon_ipc::Convertable>(v: &T) {
		let mut b = gluon_ipc::DataBuilder::new();
		v.write(&mut b).unwrap();
		assert_eq!(v.encoded_len(), b.finish(0).data().len() - 4);
	}

	#[tokio::test]
	async fn generated_encoded_len() {
		use protocol::test::{
			Arrays, BufTest, FdTestStruct, MaybeColor, Palette, Shape, TestStruct, TypesTest,
			proxied::TestEnum,
		};
		let (_node, local) = test_node();
		let fd = || std::os::fd::OwnedFd::from(std::io::pipe().unwrap().0);
		let test_struct = || TestStruct {
			string: "nya~".into(),
			id: 7,
			binder_ref: local.clone().into(),
			position: MyVec3 {
				x: 1.0,
				y: 2.0,
				z: 3.0,
			},
		};
		assert_len(&test_struct());
		assert_len(&FdTestStruct {
			fd_1: fd(),
			fd_2: fd(),
			fd_3: fd(),
			fds: vec![fd(), fd()],
		});
		assert_len(&TestEnum::TestStruct {
			test_struct: test_struct(),
		});
		assert_len(&TestEnum::Fd { fd: fd() });
		assert_len(&TestEnum::EmptyVariant);
		assert_len(&Palette {
			primary: MyColor::Red,
			secondary: MyColor::Blue,
		});
		assert_len(&MaybeColor {
			color: Some(MyColor::Green),
		});
		assert_len(&MaybeColor { color: None });
		assert_len(&TypesTest {
			flag: true,
			byte: 1,
			ushort: 2,
			uint: 3,
			ulong: 4,
			sbyte: 5,
			short: 6,
			int: 7,
			long: 8,
			float: 9.0,
			double: 10.0,
			string: "eleven".into(),
		});
		assert_len(&vec![
			Shape::Circle { radius: 1.0 },
			Shape::Named { name: "hi".into() },
			Shape::Empty,
		]);
		assert_len(&Arrays {
			bytes: [1, 2, 3, 4],
			names: ["a".into(), "bc".into()],
			colors: [MyColor::Red, MyColor::Green],
		});
		assert_len(&BufTest {
			raw: gluon_ipc::Buf::from_bytes(b"raw").unwrap(),
			shapes: gluon_ipc::Buf::new(&vec![Shape::Empty]).unwrap(),
			colors: gluon_ipc::Buf::new(&vec![]).unwrap(),
			maybe: Some(gluon_ipc::Buf::from_bytes(b"").unwrap()),
		});
	}

	#[test]
	fn generated_bufferable() {
		use protocol::test::{EXTERNAL_PROTOCOL, Palette, Shape};
		let buf = gluon_ipc::Buf::new(&vec![Shape::Named { name: "hi".into() }; 2000]).unwrap();
		assert!(buf.len() > gluon_ipc::MAX_MESSAGE_SIZE as u64);
		assert_eq!(
			buf.decode().unwrap(),
			vec![Shape::Named { name: "hi".into() }; 2000]
		);
		let v = Palette {
			primary: MyColor::Green,
			secondary: MyColor::Blue,
		};
		assert_eq!(gluon_ipc::Buf::new(&v).unwrap().decode().unwrap(), v);

		let bufferable = |n: &str| {
			EXTERNAL_PROTOCOL
				.types
				.iter()
				.find(|t| t.name == n)
				.unwrap()
				.bufferable
		};
		assert!(bufferable("Palette") && bufferable("Shape") && bufferable("Color"));
		assert!(
			!bufferable("TestStruct") && !bufferable("FdTestStruct") && !bufferable("TestEnum")
		);
	}
}
