///| EDSM への問い合わせ待ちのジャンプを溜める FIFO。
/// 上限を超えたら古いものから捨てる。
pub(all) struct Queue {
  capacity : Int
  items : Array[Jump]
}

///|
pub fn Queue::new(capacity : Int) -> Queue {
  Queue::{ capacity, items: [] }
}

///| push は末尾へ積む。上限を超えたら先頭(最も古いもの)を捨てる。
pub fn Queue::push(self : Queue, jump : Jump) -> Unit {
  if self.items.length() >= self.capacity {
    let _ = self.items.remove(0)

  }
  self.items.push(jump)
}

///|
pub fn Queue::pop(self : Queue) -> Jump? {
  if self.items.length() == 0 {
    None
  } else {
    Some(self.items.remove(0))
  }
}

///|
pub fn Queue::length(self : Queue) -> Int {
  self.items.length()
}

///| summary は溜まっているジャンプを 1 行にまとめる(on-stop の報告用)。
pub fn Queue::summary(self : Queue) -> String {
  let parts = self.items.map(fn(j) {
    "\{j.system} (\{format_ly(j.distance)} ly)"
  })
  parts.join(", ")
}
