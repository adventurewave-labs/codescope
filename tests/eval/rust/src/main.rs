use crate::store::Store;
use crate::cache::Cache;
fn run(c: &mut Cache) {
    let s = Store::open(); // @eval open=Store::open
    s.put(); // @eval put=Store::put
    c.put(); // @eval put=Cache::put
    cache::helper(); // @eval helper=helper
    store::open(); // @eval open=open@src/store.rs
    let v: Vec<u8> = Vec::new(); // @eval new=-
    v.len(); // @eval len=-
    local(); // @eval local=local
}
fn local() {}
struct App { store: Store }
impl App {
    fn save(&self) {
        self.store.put(); // @eval put=Store::put
    }
}
fn area_of(s: &dyn Shape) -> f64 {
    s.area() // @eval area=Shape::area
}
fn chain() {
    Store::open().put(); // @eval put=Store::put
}
