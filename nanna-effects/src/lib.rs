//! In-code declarations of the state assets a piece of code touches.
//!
//! The repository manifest `.nanna/effects.toml` names the assets; this crate
//! lets ownership be declared next to the code instead. Every [`touches!`]
//! invocation registers a [`Touch`] through `inventory`, and
//! [`declared`] returns everything linked into the running binary.
//!
//! ```
//! nanna_effects::touches!("db.orders");
//!
//! let touch = nanna_effects::declared()
//!     .into_iter()
//!     .find(|touch| touch.asset == "db.orders")
//!     .unwrap();
//! assert_eq!(touch.module_path, module_path!());
//! assert_eq!(touch.file, file!());
//! assert!(touch.line > 0);
//! ```

#[doc(hidden)]
pub use inventory;

/// One declaration that the code at `file:line` touches `asset`.
///
/// ```
/// use nanna_effects::Touch;
///
/// let touch = Touch::new("db.users", "api::auth", "api/src/auth.rs", 12);
/// assert_eq!(touch.asset, "db.users");
/// assert_eq!(touch.to_string(), "api/src/auth.rs:12 (api::auth) touches db.users");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Touch {
    pub asset: &'static str,
    pub module_path: &'static str,
    pub file: &'static str,
    pub line: u32,
}

impl Touch {
    pub const fn new(
        asset: &'static str,
        module_path: &'static str,
        file: &'static str,
        line: u32,
    ) -> Self {
        Self {
            asset,
            module_path,
            file,
            line,
        }
    }
}

impl std::fmt::Display for Touch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}:{} ({}) touches {}",
            self.file, self.line, self.module_path, self.asset
        )
    }
}

inventory::collect!(Touch);

/// Every [`Touch`] registered in the running binary, sorted and deduplicated.
///
/// ```
/// nanna_effects::touches!("db.sessions");
/// let all = nanna_effects::declared();
/// assert!(all.windows(2).all(|pair| pair[0] < pair[1]));
/// assert!(all.iter().any(|touch| touch.asset == "db.sessions"));
/// ```
pub fn declared() -> Vec<Touch> {
    let mut all: Vec<Touch> = inventory::iter::<Touch>().copied().collect();
    all.sort();
    all.dedup();
    all
}

/// Declare that the surrounding module touches the named asset.
///
/// Expands to an item, so it is used at module level.
///
/// ```
/// nanna_effects::touches!("db.invoices");
/// assert!(nanna_effects::declared().iter().any(|t| t.asset == "db.invoices"));
/// ```
#[macro_export]
macro_rules! touches {
    ($asset:literal) => {
        $crate::inventory::submit! {
            $crate::Touch::new($asset, module_path!(), file!(), line!())
        }
    };
}
