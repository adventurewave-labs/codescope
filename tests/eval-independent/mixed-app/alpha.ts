// TS side: broken import. './ghost' does not exist anywhere in this corpus,
// and beta/gamma already exist as PYTHON symbols in beta.py. A heuristic
// matcher could cross-bind ts imports to the python symbols; the honest
// ground truth for a broken TS import is "unbound".
import { ghost } from './ghost';
import { beta as tsBeta } from './ghost';

export function runTs(seed: number): number {
  return ghost(seed) + tsBeta(seed); // @eval ghost=-
}
