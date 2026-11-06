// edition:2024
#![allow(unused_imports)]

// One module per statement must NOT fire.
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::collections::hash_map::{self as hash_map_module, DefaultHasher};
use std::collections::{BTreeMap, btree_map};
use std::fmt::Write as FmtWrite;
use std::io::*;

// A path inside the braces reaches into a submodule and must fire.
use std::collections::{HashSet, hash_set::Iter};

// A nested group behind a path must fire.
use std::collections::{btree_set::{IntoIter, Range}};

// A glob behind a path must fire.
use std::collections::{vec_deque::*};

// A brace-only root spanning modules must fire on every entry.
use {std::rc::Rc, std::sync::Arc};

// Only the offending entries are labeled, however deep the group.
use std::{cell::{Cell, RefCell}, convert::Infallible, time::{Duration, Instant}};

// `#[allow]` at the site must suppress the lint.
#[allow(multi_module_use)]
use std::{any::Any, borrow::Cow};

fn main() {}
