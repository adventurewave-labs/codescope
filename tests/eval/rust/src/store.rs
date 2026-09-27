pub struct Store;
impl Store {
    pub fn open() -> Store { Store }
    pub fn put(&self) { self.flush(); } // @eval flush=Store::flush
    fn flush(&self) {}
}
pub fn open() {}
