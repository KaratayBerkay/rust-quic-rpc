import 'reflect-metadata';
import { NestFactory } from '@nestjs/core';
import { AppModule } from './app.module';
import { loadConfig } from './config';

async function bootstrap(): Promise<void> {
  const config = loadConfig();
  const app = await NestFactory.create(AppModule);
  app.enableCors(); // dev convenience; tighten before production
  await app.listen(config.port);
  console.log(`rvp-nest listening on :${config.port}`);
}

bootstrap().catch((err: unknown) => {
  console.error('fatal:', err);
  process.exit(1);
});
