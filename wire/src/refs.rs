use std::{ops::Deref, sync::Arc};

use strong_ipc::{DeathNotifier, Handler, Node, NodeError, Ref, RefFsBinding};

use crate::{HandledBy, Interface, IntoHandler, Liveness, ToRef};

/// A proxy paired with the handler behind it, for a node made in *this* process.
///
/// A plain proxy is a [`Ref`] and nothing else, which is the whole truth about one a peer
/// handed us but throws away what we knew about one we built ourselves. This keeps both:
/// the proxy to send through, and the `Arc<H>` the node is already feeding. No lookup, no
/// `Option` — the pairing is a fact of construction rather than something to go and check.
///
/// The [`HandledBy<H>`] bound is what makes that pairing honest. It is the same bound that
/// decides what may be *put* behind an interface, so a `LocalRef<Test, H>` can only exist
/// for an `H` answering `Test`'s methods — checked when it is built, not downcast when it
/// is read.
///
/// [`Deref`]s to the `Arc<H>`, so the handler's own methods and fields are reached bare and
/// the interface's go through [`LocalRef::proxy`]. Unlike [`Node`], which refuses the same
/// deref precisely because `node.clone()` would silently hand back a share of its handler,
/// that is safe here: `LocalRef` has its own `Clone`, and an inherent impl wins over a
/// deref, so `local.clone()` is another `LocalRef`.
///
/// **Do not store one of these in the handler it points at.** `Arc<H>` → `LocalRef` →
/// `Arc<H>` is a cycle and the handler never drops. Keep the bare proxy for a
/// self-reference — [`LocalRef::proxy`] hands it over.
pub struct LocalRef<I, H> {
	proxy: I,
	handler: Arc<H>,
}

impl<I: RefExt + HandledBy<H>, H: Handler> LocalRef<I, H> {
	/// Pairs a proxy with the handler behind it.
	///
	/// Nothing here checks that `proxy` actually leads to `handler` — the constructors on
	/// [`RefExt`] are the ones that know, and this is how they say so.
	pub fn new(proxy: I, handler: Arc<H>) -> Self {
		Self { proxy, handler }
	}

	/// The handler behind this proxy.
	///
	/// No lookup and no `Option`, unlike [`RefExt::local_handler`]: this is the `Arc` the
	/// node is running, carried here since it was built.
	pub fn handler(&self) -> &Arc<H> {
		&self.handler
	}

	/// The proxy, for calling this interface's methods and for anywhere a peer's ref goes.
	pub fn proxy(&self) -> &I {
		&self.proxy
	}

	/// Drops the handler share and keeps the proxy.
	pub fn into_proxy(self) -> I {
		self.proxy
	}
}

/// To the handler, as binderbinder's `BinderObjectRef` did — see the type's own docs for
/// why this is safe here and not on [`Node`].
impl<I: RefExt + HandledBy<H>, H: Handler> Deref for LocalRef<I, H> {
	type Target = Arc<H>;
	fn deref(&self) -> &Arc<H> {
		&self.handler
	}
}

/// Hand-written rather than derived, and rebuilding the proxy from its ref rather than
/// cloning it: that costs the same `Arc` bump and spares [`Interface`] a `Clone` supertrait
/// it would otherwise need for no other reason.
impl<I: RefExt + HandledBy<H>, H: Handler> Clone for LocalRef<I, H> {
	fn clone(&self) -> Self {
		Self {
			proxy: I::from_ref(self.proxy.to_ref()),
			handler: self.handler.clone(),
		}
	}
}

/// Prints the proxy, since the handler is not obliged to be `Debug` and the ref is the
/// identity anyway.
impl<I: RefExt + HandledBy<H> + std::fmt::Debug, H: Handler> std::fmt::Debug for LocalRef<I, H> {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("LocalRef")
			.field("proxy", &self.proxy)
			.finish_non_exhaustive()
	}
}

/// So a `LocalRef` goes straight into an untyped-ref parameter or [`DataBuilder::write_ref`].
impl<I: RefExt + HandledBy<H>, H: Handler> ToRef for LocalRef<I, H> {
	fn to_ref(&self) -> Ref {
		self.proxy.to_ref()
	}
}

/// The generated `From<LocalRef<Proxy, H>> for Proxy` is the one that feeds the
/// `impl Into<Proxy>` parameters; this is the untyped end of the same road.
impl<I: RefExt + HandledBy<H>, H: Handler> From<LocalRef<I, H>> for Ref {
	fn from(value: LocalRef<I, H>) -> Ref {
		value.proxy.to_ref()
	}
}

impl<I: RefExt + HandledBy<H>, H: Handler> Liveness for LocalRef<I, H> {
	fn alive(&self) -> bool {
		Liveness::alive(&self.proxy.to_ref())
	}
	fn death_notifier(&self) -> DeathNotifier {
		Liveness::death_notifier(&self.proxy.to_ref())
	}
}

/// Identity is the proxy's, which is the ref's — the handler share says nothing about
/// which node this is.
impl<I: RefExt + HandledBy<H> + PartialEq, H: Handler> PartialEq for LocalRef<I, H> {
	fn eq(&self, other: &Self) -> bool {
		self.proxy == other.proxy
	}
}
impl<I: RefExt + HandledBy<H> + Eq, H: Handler> Eq for LocalRef<I, H> {}
impl<I: RefExt + HandledBy<H> + std::hash::Hash, H: Handler> std::hash::Hash for LocalRef<I, H> {
	fn hash<S: std::hash::Hasher>(&self, state: &mut S) {
		self.proxy.hash(state);
	}
}

/// Everything you can do with an interface's proxy besides call its methods: reach a node
/// that already exists, put a handler behind a new one, or publish one at a path.
///
/// The handler constructors take [`IntoHandler`], so a handler you already share elsewhere
/// goes in as the `Arc` and one you don't goes in bare, and they bound `H` by
/// [`HandledBy<H>`] so only a handler that actually answers this interface's methods
/// is accepted. A handler for some other interface doesn't fail a check — the call simply
/// doesn't resolve for it.
pub trait RefExt: Interface + Sized {
	/// Wraps a ref you already have.
	///
	/// Only use this when you know the ref leads to something implementing this interface,
	/// else the consequences are for you to find out.
	fn from_ref(obj: Ref) -> Self;

	/// Connects to the [`RefFsBinding`] listening at `path`.
	///
	/// The other side of the bootstrap problem: a path is the one name that isn't itself a
	/// capability, so this is how you get a first ref without anyone handing you one.
	/// Nothing checks that whatever is listening speaks this interface.
	fn connect(
		path: impl AsRef<std::path::Path> + Send,
	) -> impl Future<Output = Result<Self, NodeError>> + Send {
		async move { Ok(Self::from_ref(Ref::connect(path).await?)) }
	}

	/// Publishes this proxy at `path`, so anyone who can open it gets a ref by
	/// [`RefExt::connect`]ing.
	///
	/// The other half of the bootstrap problem, and the only door into a process that
	/// isn't already a capability: every connection served here hands out a ref to the
	/// same node, and nothing about the path is checked beyond the filesystem's own
	/// permissions.
	///
	/// Keep the binding — dropping it stops the accept loop, and refs already handed out
	/// stay live. It does not unlink `path`, so a stale socket file left by a killed
	/// process reports [`std::io::ErrorKind::AddrInUse`] until something removes it.
	fn bind(&self, path: impl AsRef<std::path::Path>) -> Result<RefFsBinding, NodeError> {
		Ok(RefFsBinding::new(self.to_ref(), path)?)
	}

	/// Runs `handler` on a new node reached through the returned [`LocalRef`].
	///
	/// A [`LocalRef`] rather than a bare proxy because this is the one call that *knows*
	/// what is behind the ref it hands back — throwing that away here is what forced every
	/// caller to either carry the `Arc<H>` in a second variable or go and look it up again
	/// with [`Self::local_handler`].
	///
	/// Keep the node. It *is* the node, and dropping it hangs its socket up, so the
	/// `LocalRef` returned beside it goes dead — it holds a share of the handler, but that
	/// keeps the handler alive, not the node. Use [`Self::new_service`] when you would
	/// rather the refs decided that.
	fn new_node<H: Handler>(
		handler: impl IntoHandler<H>,
	) -> Result<(Node<H>, LocalRef<Self, H>), NodeError>
	where
		Self: HandledBy<H>,
	{
		let (node, node_ref) = Node::new_raw(handler.into_handler())?;
		let local = LocalRef::new(Self::from_ref(node_ref), node.handler().clone());
		Ok((node, local))
	}

	/// [`Self::new_node`] for a handler nothing is going to hold onto.
	///
	/// Hands the node's lifetime straight to its refs through [`Node::to_service`], so
	/// there is no node to keep and the [`LocalRef`] is the whole result. It lives until
	/// the last ref to it goes, and there is no getting it back to stop it earlier.
	fn new_service<H: Handler>(handler: impl IntoHandler<H>) -> Result<LocalRef<Self, H>, NodeError>
	where
		Self: HandledBy<H>,
	{
		let (node, local) = Self::new_node(handler)?;
		node.to_service();
		Ok(local)
	}

	/// The handler behind this proxy, if it leads to a node in *this* process.
	///
	/// A proxy a peer handed you is normally opaque — you call its methods and the wire
	/// decides what happens. But a process that is both ends of an interface handed out
	/// the node in the first place, and a ref it gets back is recognised on arrival, so
	/// the handler is a hash lookup away rather than a round trip through its own wire
	/// format. That is the whole shortcut, and why it is behind a feature.
	///
	/// The [`HandledBy<H>`] bound is doing real work here: it is what stops this being a
	/// blind downcast. Asking a `Spatial` for a handler that only answers `Field`'s
	/// methods doesn't return `None`, it doesn't compile — the same bound that decides
	/// what may be *put* behind this interface decides what may be recovered from it.
	///
	/// `None` means every way this can fail to be a handler you can have: the proxy leads
	/// to another process, its node is gone, or it is some other `H` entirely.
	#[cfg(feature = "local-handlers")]
	fn local_handler<H: Handler>(&self) -> Option<Arc<H>>
	where
		Self: HandledBy<H>,
	{
		self.to_ref().local_handler::<H>()
	}

	/// [`Self::local_handler`], but the ref you hand in becomes the proxy instead of being
	/// dropped on the floor.
	///
	/// The lookup needs a ref and so does the proxy, so this is the same one call with
	/// nothing thrown away: `obj` moves into the [`LocalRef`] rather than being cloned back
	/// out of it afterwards. What comes back can be *called* as well as read from, which is
	/// the difference from a bare `Arc<H>`.
	///
	/// `None` means what it always did — the ref leads to another process, its node is
	/// gone, or it is some other `H` entirely.
	#[cfg(feature = "local-handlers")]
	fn local_from_ref<H: Handler>(obj: Ref) -> Option<LocalRef<Self, H>>
	where
		Self: HandledBy<H>,
	{
		let handler = obj.local_handler::<H>()?;
		Some(LocalRef::new(Self::from_ref(obj), handler))
	}

	/// [`Self::local_from_ref`] for a proxy you are already holding — the typed answer to
	/// "is this one of mine?" for a ref that came back in off the wire.
	#[cfg(feature = "local-handlers")]
	fn as_local<H: Handler>(&self) -> Option<LocalRef<Self, H>>
	where
		Self: HandledBy<H>,
	{
		Self::local_from_ref(self.to_ref())
	}
}
