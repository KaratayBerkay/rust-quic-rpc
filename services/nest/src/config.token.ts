import { InjectionToken } from '@nestjs/common';
import type { AppConfig } from './config';

export const APP_CONFIG: InjectionToken<AppConfig> = 'APP_CONFIG';
