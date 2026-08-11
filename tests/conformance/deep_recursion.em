fn sum_to(n) {
    if n == 0 { 0 } else { n + sum_to(n - 1) }
}

sum_to(50);
