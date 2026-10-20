use thiserror::Error;

use crate::param_name::ParamName;

#[derive(Clone, Debug, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum CaveatContextError {
    #[error("parameter {parameter} is not declared for this caveat")]
    Undeclared { parameter: ParamName },
    #[error("parameter {parameter} was supplied twice")]
    Duplicate { parameter: ParamName },
    #[error("parameter {parameter} nests an integer above 2^53, which a protobuf Struct cannot carry exactly")]
    NestedIntegerPrecision { parameter: ParamName },
    #[error("parameter {parameter} is not a finite number")]
    NonFiniteDouble { parameter: ParamName },
    #[error("caveat context encodes to {encoded} bytes, over the budget of {budget}")]
    OverBudget { encoded: usize, budget: usize },
}
