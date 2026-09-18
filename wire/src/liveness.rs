use std::pin::Pin;

use strong_ipc::{DeathNotifier, Handler, Node, Ref};

/// Liveness of the node a ref points to.
pub trait Liveness {
	/// Whether the node is (as far as we know) still alive.
	fn alive(&self) -> bool {
		!self.death_notifier().is_dead()
	}
	/// This death as an owned handle, to hold and wait on somewhere else.
	///
	/// The one method worth implementing: [`Self::death_notification`] falls out of it,
	/// and unlike that one-shot future this can be cloned, waited on more than once, and
	/// asked with [`DeathNotifier::is_dead`] without awaiting at all.
	///
	/// What holding one says about the lifetime of what it watches depends on which
	/// variant comes back — see [`DeathNotifier`].
	fn death_notifier(&self) -> DeathNotifier;
	/// Future that resolves once the node has died.
	fn death_notification(&self) -> Pin<Box<dyn Future<Output = ()> + Send>> {
		let notifier = self.death_notifier();
		Box::pin(async move { notifier.wait().await })
	}
}

impl Liveness for Ref {
	fn alive(&self) -> bool {
		!self.is_dead()
	}
	fn death_notifier(&self) -> DeathNotifier {
		Ref::death_notifier(self)
	}
}

/// A node in this process, which is dead once nothing can reach it any more.
impl<H: Handler> Liveness for Node<H> {
	fn alive(&self) -> bool {
		!self.is_dead()
	}
	fn death_notifier(&self) -> DeathNotifier {
		// Spelled as a path: the inherent method and this one share a name, and the
		// inherent one is what should answer here.
		Node::death_notifier(self)
	}
}

/// A death already held apart from whatever it belongs to.
///
/// The identity impl, so anything handing out a notifier can be waited on the same way as
/// the notifier itself.
impl Liveness for DeathNotifier {
	fn alive(&self) -> bool {
		!self.is_dead()
	}
	fn death_notifier(&self) -> DeathNotifier {
		self.clone()
	}
}
