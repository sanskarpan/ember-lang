fn make_counter() {
    let mut count = 0;
    || { count = count + 1; count }
}

let counter = make_counter();
counter();
counter();
counter();
