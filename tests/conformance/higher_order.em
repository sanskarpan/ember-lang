fn apply_twice(f, x) { f(f(x)) }
fn add_one(n) { n + 1 }

apply_twice(add_one, 5);
