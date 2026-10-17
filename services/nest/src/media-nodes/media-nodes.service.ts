import { Inject, Injectable, Logger, OnModuleDestroy, OnModuleInit } from '@nestjs/common';
import { APP_CONFIG } from '../config.token';
import { AppConfig } from '../config';

/** One media node the control plane may pin a room to (P8a, plan §2/§5). */
export interface MediaNode {
  id: string;
  /** `host:port` of the media server (QUIC/UDP). Used by clients to connect. */
  address: string;
}

/**
 * Media-node directory (P8a, plan §2/§5).
 *
 * Supplies the round-robin pin list for `RoomsService`. Sources, in order of
 * precedence at read time:
 * 1. `RVP_MEDIA_NODES_URL` — polled every 15s (`{"nodes":[{id,address}]}`).
 * 2. `MEDIA_NODES` — static `id@host:port` list from env.
 * 3. Empty — no pinning (single-node default; grants carry no `media_node`).
 *
 * A failed poll keeps the last good list (control plane keeps routing with
 * stale but usable info rather than dropping pinning entirely).
 */
@Injectable()
export class MediaNodesService implements OnModuleInit, OnModuleDestroy {
  private readonly logger = new Logger(MediaNodesService.name);
  private nodes: MediaNode[] = [];
  private timer: NodeJS.Timeout | null = null;

  constructor(@Inject(APP_CONFIG) private readonly config: AppConfig) {
    this.nodes = this.parseEnvList(config.mediaNodes);
  }

  async onModuleInit(): Promise<void> {
    if (this.config.mediaNodesUrl) {
      await this.pollOnce();
      this.timer = setInterval(() => void this.pollOnce(), 15_000);
      this.timer.unref?.();
    }
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
