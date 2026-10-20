#[macro_export]
macro_rules! object_id {
    ($literal:literal) => {
        $crate::__private::object_id_impl!($crate, $literal)
    };
}

#[macro_export]
macro_rules! object_type {
    ($literal:literal) => {
        $crate::__private::object_type_impl!($crate, $literal)
    };
}

#[macro_export]
macro_rules! principal_kind {
    ($literal:literal) => {
        $crate::__private::principal_kind_impl!($crate, $literal)
    };
}

#[cfg(feature = "spicedb")]
#[macro_export]
macro_rules! relation_name {
    ($literal:literal) => {
        $crate::__private::relation_name_impl!($crate, $literal)
    };
}

#[cfg(feature = "spicedb")]
#[macro_export]
macro_rules! param_name {
    ($literal:literal) => {
        $crate::__private::param_name_impl!($crate, $literal)
    };
}
