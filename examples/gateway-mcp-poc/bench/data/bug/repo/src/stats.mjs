// Small statistics helpers.

export function mean(xs) {
  return xs.reduce((a, b) => a + b, 0) / xs.length;
}

// The median of a list of numbers, in any order.
export function median(xs) {
  const mid = Math.floor(xs.length / 2);
  return xs.length % 2 ? xs[mid] : (xs[mid - 1] + xs[mid]) / 2;
}
