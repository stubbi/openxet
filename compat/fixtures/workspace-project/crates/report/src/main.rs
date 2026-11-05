//! Reads builds as JSON on stdin and prints their total size.

use std::io::Read;

fn main() {
    let mut input = String::new();
    std::io::stdin()
        .read_to_string(&mut input)
        .expect("stdin is readable");
    let builds = codec::decode(&input).expect("stdin is a JSON list of builds");
    println!("{}", codec::bytes_label(model::Build::total_bytes(&builds)));
}
