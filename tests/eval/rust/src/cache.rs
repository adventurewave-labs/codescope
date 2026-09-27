pub struct Cache { items: Vec<u8> }
impl Cache {
    pub fn new() -> Cache { Cache { items: Vec::new() } } // @eval new=-
    pub fn put(&mut self) {
        self.items.push(1); // @eval push=-
        self.flush(); // @eval flush=Cache::flush
    }
    fn flush(&mut self) {}
}
pub trait Shape { fn area(&self) -> f64; }
pub struct Sq;
impl Shape for Sq { fn area(&self) -> f64 { 1.0 } }
pub fn helper() {}
