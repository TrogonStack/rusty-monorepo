// edition:2024
#![allow(unused)]

struct Bug {
    priority: u8,
}

fn parse(raw: &str) -> Result<Bug, ()> {
    Ok(Bug { priority: raw.len() as u8 })
}

fn assign(_bug: Bug) {}

fn report(_value: u32) {}

fn chain_with_closures(descriptions: &[&str]) {
    for bug in descriptions
        .iter()
        .filter_map(|description| parse(description).ok())
        .filter(|bug| bug.priority <= 1)
    {
        assign(bug);
    }
}

fn single_closure(values: Vec<u32>) {
    for doubled in values.iter().map(|value| value * 2) {
        report(doubled);
    }
}

fn closure_before_plain_adapter(values: Vec<u32>) {
    for (index, value) in values.iter().filter(|value| **value > 1).enumerate() {
        report(index as u32 + *value);
    }
}

fn plain_adapters(values: Vec<u32>, others: Vec<u32>, text: &str) {
    for (index, value) in values.iter().enumerate() {
        report(index as u32 + *value);
    }
    for value in values.iter().rev() {
        report(*value);
    }
    for line in text.lines() {
        report(line.len() as u32);
    }
    for (left, right) in values.iter().zip(others.iter()) {
        report(*left + *right);
    }
}

fn plain_iterables(values: Vec<u32>) {
    for value in &values {
        report(*value);
    }
    for index in 0..values.len() {
        report(index as u32);
    }
    let doubled = values.iter().map(|value| value * 2);
    for value in doubled {
        report(value);
    }
}

fn closure_in_body_only(values: Vec<u32>) {
    for value in values.iter() {
        let doubled: Vec<u32> = values.iter().map(|other| other * value).collect();
        report(doubled.len() as u32);
    }
}

fn macro_expansion() {
    for value in vec![1u32, 2, 3] {
        report(value);
    }
}

#[allow(iterator_chain_in_for_header)]
fn suppressed(values: Vec<u32>) {
    for doubled in values.iter().map(|value| value * 2) {
        report(doubled);
    }
}

fn closure_on_option(maybe: Option<Vec<u32>>) {
    for value in maybe.map(|values| values).unwrap_or_default() {
        report(value);
    }
}

fn main() {}
