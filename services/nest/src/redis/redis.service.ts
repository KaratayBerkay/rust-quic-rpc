import {
  Inject,
  Injectable,
  Logger,
  OnModuleDestroy,
  OnModuleInit,
} from '@nestjs/common';
import { createClient, type RedisClientType } from 'redis';
import { APP_CONFIG } from '../config.token';
import { AppConfig } from '../config';

/**
 * Optional Redis connection (P8b). Returns `null` when `REDIS_URL` is unset
 * so single-node deployments keep working with no Redis at all. All call
 * sites null-check the result.
 *
 * Shared by `MediaNodesService` (directory reads) and `RevocationService`
 * (publish kicks).
 */
@Injectable()
export class RedisService implements OnModuleInit, OnModuleDestroy {
  private readonly logger = new Logger(RedisService.name);
  private client: RedisClientType | null = null;

  constructor(@Inject(APP_CONFIG) private readonly config: AppConfig) {}

  async onModuleInit(): Promise<void> {
    if (!this.config.redisUrl) {
      this.logger.log('REDIS_URL unset — running single-node (no Redis)');
      return;
    }
    this.client = createClient({ url: this.config.redisUrl }) as RedisClientType;
    this.client.on('error', (err: Error) =>
      this.logger.warn(`redis error: ${err.message}`),
    );
    try {
      await this.client.connect();
      this.logger.log(`redis connected: ${this.config.redisUrl}`);
    } catch (err) {
      // Do not crash the control plane: fall back to single-node behaviour.
      this.logger.warn(
        `redis connect failed, falling back to single-node: ${err}`,
      );
      this.client = null;
    }
  }

  async onModuleDestroy(): Promise<void> {
    if (this.client) {
      await this.client.quit().catch(() => undefined);
      this.client = null;
    }
  }

  /** Connected client, or `null` when Redis is disabled/unavailable. */
  get(): RedisClientType | null {
    return this.client;
  }
}
