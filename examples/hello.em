fn fact(n) {
    if n == 0 { 1 } else { n * fact(n - 1) }
}

let x = fact(5);
print(x);
