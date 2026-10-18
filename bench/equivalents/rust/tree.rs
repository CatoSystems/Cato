enum Tree {
    Node(i64, Box<Tree>, Box<Tree>),
    Leaf,
}

fn build(lo: i64, hi: i64) -> Tree {
    if lo > hi {
        Tree::Leaf
    } else {
        let mid = (lo + hi) / 2;
        Tree::Node(mid, Box::new(build(lo, mid - 1)), Box::new(build(mid + 1, hi)))
    }
}

fn sum(t: &Tree) -> i64 {
    match t {
        Tree::Node(v, l, r) => v + sum(l) + sum(r),
        Tree::Leaf => 0,
    }
}

fn main() {
    println!("{}", sum(&build(0, 100_000)));
}
