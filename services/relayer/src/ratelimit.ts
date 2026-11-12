/** Sliding-window limiter: at most `max` hits per `windowMs` for each key. */
export class RateLimit {
  private hits = new Map<string, number[]>();
  constructor(private max: number, private windowMs: number, private now: () => number = Date.now) {}

  /** Records a hit and returns true if it is allowed. A refused hit is not recorded. */
  take(key: string): boolean {
    const t = this.now();
    const live = (this.hits.get(key) ?? []).filter((x) => t - x < this.windowMs);
    if (live.length >= this.max) {
      this.hits.set(key, live);
      return false;
    }
    live.push(t);
    this.hits.set(key, live);
    if (this.hits.size > 50_000) this.sweep(t);
    return true;
  }

  /** Seconds until a hit would be allowed again. */
  retryAfter(key: string): number {
    const t = this.now();
    const live = (this.hits.get(key) ?? []).filter((x) => t - x < this.windowMs);
    return live.length < this.max ? 0 : Math.ceil((this.windowMs - (t - live[0]!)) / 1000);
  }

  private sweep(t: number) {
    for (const [k, v] of this.hits) if (v.every((x) => t - x >= this.windowMs)) this.hits.delete(k);
  }
}
