import { Queue } from "./queue";

function start() {
  const q = new Queue(); // @eval Queue=Queue
  q.push(1); // @eval push=Queue::push
  drain(q); // @eval drain=drain
  JSON.stringify(q); // @eval stringify=-
  setTimeout(start, 10); // @eval setTimeout=-
}
