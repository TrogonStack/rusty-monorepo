/// What a headroom threshold defaults to unless the operator says otherwise.
///
/// A suite whose worst-case (95% lower-bound) with_skill pass rate has already cleared
/// nine in ten leaves at most one in ten of room for a change to show up in, and more
/// attempts only tighten that floor further. A suite that trips this warning at 0.9 is
/// not going to grow room back on its own; it needs harder cases, not more draws.
pub const DEFAULT_HEADROOM_THRESHOLD: f64 = 0.9;
