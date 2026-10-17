import { Injectable, Logger } from '@nestjs/common';
import { RedisService } from './redis.service';
import { NODE_KEY_GLOB, type MediaNode } from './keys';

/**
 * Redis-backed media-node directory reader (P8b).
 *
 * Media nodes self-register one `rvp:node:<id>` key each (JSON entry + TTL)
 * and refresh it on a heartbeat. This service snapshots all live keys so
 * `MediaNodesService` can pin rooms round-robin across nodes without a
 * static config.
 *
 * Returns `[]` when Redis is disabled or unavailable — callers fall back to
 * the P8a static/polled list.
 */
@Injectable()
export class DirectoryService {
  private readonly logger = new Logger(DirectoryService.name);

  constructor(private readonly redis: RedisService) {}

  /** Snapshot of currently-live media nodes (empty when no Redis). */
  async listNodes(): Promise<MediaNode[]> {
    const client = this.redis.get();
    if (!client) {
      return [];
    }
    try {
      const nodes: MediaNode[] = [];
      let cursor = 0;
      do {
        const page = await client.scan(cursor, {
          MATCH: NODE_KEY_GLOB,
          COUNT: 100,
        });
        cursor = page.cursor;
        if (page.keys.length === 0) continue;
        const values = await client.mGet(page.keys);
        for (const value of values) {
          if (!value) continue;
          try {
            const parsed = JSON.parse(value) as MediaNode;
            if (parsed?.id && parsed?.address) {
              nodes.push({ id: parsed.id, address: parsed.address });
            }
          } catch {
            // Skip a corrupt entry rather than failing the whole list.
          }
        }
      } while (cursor !== 0);
      return nodes;
    } catch (err) {
      this.logger.warn(`directory read failed, returning empty: ${err}`);
      return [];
    }
  }
}
