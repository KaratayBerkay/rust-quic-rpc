import { Inject, Injectable, Logger, OnModuleDestroy, OnModuleInit } from '@nestjs/common';
import { APP_CONFIG } from '../config.token';
import { AppConfig } from '../config';
import { DirectoryService } from '../redis/directory.service';
import type { MediaNode } from '../redis/keys';

export type { MediaNode };

/**
 * Media-node directory (P8a, plan §2/§5).
 *
 * Supplies the round-robin pin list for `RoomsService`. Sources, in order of
 * precedence at read time:
 * 1. Redis directory (`rvp:node:<id>`, P8b) — media nodes self-register with a
 *    TTL, so this is live and self-healing. Empty when Redis is disabled.
 * 2. `RVP_MEDIA_NODES_URL` — polled every 15s (`{"nodes":[{id,address}]}`).
 * 3. `MEDIA_NODES` — static `id@host:port` list from env.
 * 4. Empty — no pinning (single-node default; grants carry no `media_node`).
 *
 * A failed poll keeps the last good list (control plane keeps routing with
 * stale but usable info rather than dropping pinning entirely).
 */
@Injectable()
export class MediaNodesService implements OnModuleInit, OnModuleDestroy {
  private readonly logger = new Logger(MediaNodesService.name);
  private nodes: MediaNode[] = [];
  private timer: NodeJS.Timeout | null = null;

  constructor(
    @Inject(APP_CONFIG) private readonly config: AppConfig,
    private readonly directory: DirectoryService,
  ) {
    this.nodes = this.parseEnvList(config.mediaNodes);
  }

  async onModuleInit(): Promise<void> {
    // P8b: Redis directory is polled on the same 15s cadence as the HTTP
    // source (both feed the same `nodes` list; Redis wins when non-empty).
    this.timer = setInterval(() => void this.refresh(), 15_000);
    this.timer.unref?.();
    await this.refresh();
  }

  onModuleDestroy(): void {
    if (this.timer) {
      clearInterval(this.timer);
      this.timer = null;
    }
  }

  /** Current pin list (possibly empty = no pinning). */
  list(): MediaNode[] {
    return this.nodes.slice();
  }

  /** Pick the next node round-robin, or `null` when the list is empty. */
  next(): MediaNode | null {
    if (this.nodes.length === 0) {
      return null;
    }
    // Cheap round-robin via an incrementing cursor.
    this.cursor = (this.cursor + 1) % this.nodes.length;
    return this.nodes[this.cursor];
  }

  private cursor = -1;

  /** One refresh tick: prefer the Redis directory, else the HTTP source. */
  private async refresh(): Promise<void> {
    const fromRedis = await this.directory.listNodes();
    if (fromRedis.length > 0) {
      this.nodes = fromRedis;
      return;
    }
    if (this.config.mediaNodesUrl) {
      await this.pollOnce();
    }
  }

  /** `id@host:port` entries → `MediaNode`. Malformed entries are skipped. */
  private parseEnvList(list: string[]): MediaNode[] {
    const out: MediaNode[] = [];
    for (const raw of list) {
      const at = raw.indexOf('@');
      if (at <= 0 || at === raw.length - 1) {
        this.logger.warn(`[media-nodes] skipping malformed entry: ${raw}`);
        continue;
      }
      out.push({ id: raw.slice(0, at), address: raw.slice(at + 1) });
    }
    return out;
  }

  private async pollOnce(): Promise<void> {
    try {
      const res = await fetch(this.config.mediaNodesUrl!);
      if (!res.ok) {
        throw new Error(`HTTP ${res.status}`);
      }
      const body = (await res.json()) as { nodes?: MediaNode[] };
      const nodes = Array.isArray(body.nodes) ? body.nodes : [];
      const filtered = nodes.filter(
        (n) => typeof n?.id === 'string' && n.id.length > 0 && typeof n?.address === 'string',
      );
      this.nodes = filtered;
    } catch (err) {
      this.logger.warn(`[media-nodes] poll failed, keeping last list: ${err}`);
    }
  }
}
