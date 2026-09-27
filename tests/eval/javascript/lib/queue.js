class Queue {
  push(x) {
    this.items.push(x); // @eval push=-
    this.notify(); // @eval notify=Queue::notify
  }
  notify() {}
}
const drain = (q) => q.items.length;
module.exports = { Queue, drain };
