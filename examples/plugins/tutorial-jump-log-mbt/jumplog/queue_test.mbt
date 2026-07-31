///|
test "queue: FIFO で出し入れする" {
  let q = @jumplog.Queue::new(3)
  assert_eq(q.pop(), None)
  q.push(@jumplog.Jump::{ system: "Sol", distance: 8.19 })
  q.push(@jumplog.Jump::{ system: "Achenar", distance: 3.2 })
  assert_eq(q.length(), 2)
  assert_eq(q.pop().map(fn(j) { j.system }), Some("Sol"))
  assert_eq(q.pop().map(fn(j) { j.system }), Some("Achenar"))
  assert_eq(q.pop(), None)
}

///|
test "queue: 上限を超えたら古いものから捨てる" {
  let q = @jumplog.Queue::new(2)
  q.push(@jumplog.Jump::{ system: "A", distance: 1 })
  q.push(@jumplog.Jump::{ system: "B", distance: 2 })
  q.push(@jumplog.Jump::{ system: "C", distance: 3 })
  assert_eq(q.length(), 2)
  assert_eq(q.pop().map(fn(j) { j.system }), Some("B"))
}

///|
test "queue: summary" {
  let q = @jumplog.Queue::new(2)
  q.push(@jumplog.Jump::{ system: "Achenar", distance: 3.2 })
  assert_eq(q.summary(), "Achenar (3.20 ly)")
}
