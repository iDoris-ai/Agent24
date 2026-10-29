// Desktop shell entry for the single data-directory resolver
// (ADR-037 decision 3). Implementation lives in @agent24/data-dir so the
// other TS packages share it; this file only re-exports, behavior unchanged.

export {
  DataDirError,
  currentDataDirPlatform,
  dataDir,
  joinLiteral,
  joinLiteralFor,
  resolveDataDir,
  tryDataDir,
} from '@agent24/data-dir'
export type { DataDirEnv, DataDirErrorCode, DataDirPlatform } from '@agent24/data-dir'
