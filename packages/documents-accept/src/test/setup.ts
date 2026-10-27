// The contract's schemas are read from protocol/openapi.yaml and compiled on
// first use, once per test file (src/contract.ts). Done here, before the
// tests, that cost is not carried by whichever test first checks an answer:
// on a busy machine it alone took that test past its 5 s.

import { ContractError, conform } from '../contract'

/** Every schema the harness checks an answer against. */
const SCHEMAS = ['DocumentsCapabilities', 'DocumentsUpload', 'DocumentsJob', 'DocumentDetail', 'DocumentList', 'DocumentsTextPage', 'DocumentsFindResult', 'DocumentsExtraction']

for (const name of SCHEMAS) {
  try {
    conform(name, undefined)
  } catch (error) {
    if (!(error instanceof ContractError)) throw error
  }
}
