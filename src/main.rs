use std::time::Instant;

use mobiletypst::{load_fonts, walk_node, TypstWorld};
use typst_syntax::{highlight, parse, LinkedNode, SyntaxNode};

fn main() {
    let world = TypstWorld::new("./test".to_string());
    let document = world.compile().unwrap();
    let p = parse("#block()[= test]");

    let linked_node = LinkedNode::new(&p);
    // walk_node(&linked_node);
    for i in 0..4 {
        let start = Instant::now();
        let _ = world.compile().unwrap();
        let end = Instant::now();
        println!("Took {}ms", (end - start).as_millis());
    }
}
