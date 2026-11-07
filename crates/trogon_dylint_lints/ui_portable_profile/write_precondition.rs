// edition:2024
#![allow(dead_code)]

enum WritePrecondition {
    Any,
}

trait Decider {
    const WRITE_PRECONDITION: WritePrecondition;
}

struct Record;

impl Decider for Record {
    const WRITE_PRECONDITION: WritePrecondition = WritePrecondition::Any;
}

fn main() {}
