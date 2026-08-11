let mut i = 0;
let mut total = 0;
while i < 5 {
    total = total + i;
    i = i + 1;
}

let mut count = 0;
loop {
    if count >= 3 { break; }
    count = count + 1;
}

total + count;
