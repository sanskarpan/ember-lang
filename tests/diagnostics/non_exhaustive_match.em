type Shape = Circle(Float) | Square(Float);
fn area(s) {
    match s {
        Circle(r) => 3.14 * r * r,
    }
}
