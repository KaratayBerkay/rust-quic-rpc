import { Module } from '@nestjs/common';
import { APP_GUARD } from '@nestjs/core';
import { Pool } from 'pg';
import { loadConfig, type AppConfig } from './config';
import { APP_CONFIG } from './config.token';
import { createPool, migrate } from './db/pool';
import { KeysService } from './keys/keys.service';
import { AuthController } from './auth/auth.controller';
import { SessionGuard } from './auth/session.guard';
import { RoomsController } from './rooms/rooms.controller';
import { RoomsService } from './rooms/rooms.service';
import { KeysController } from './rooms/keys.controller';
import { GrantsController } from './grants/grants.controller';
import { GrantIssuesService } from './grants/grant-issues.service';
import { MediaNodesService } from './media-nodes/media-nodes.service';

@Module({
  controllers: [AuthController, RoomsController, KeysController, GrantsController],
  providers: [
    { provide: APP_CONFIG, useFactory: () => loadConfig() },
    {
      provide: KeysService,
      useFactory: (config: AppConfig) => KeysService.create(config),
      inject: [APP_CONFIG],
    },
    {
      provide: Pool,
      useFactory: async (config: AppConfig) => {
        const pool = createPool(config);
        await migrate(pool);
        return pool;
      },
      inject: [APP_CONFIG],
    },
    RoomsService,
    GrantIssuesService,
    MediaNodesService,
    { provide: APP_GUARD, useClass: SessionGuard },
  ],
})
export class AppModule {}
