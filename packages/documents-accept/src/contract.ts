// What a documents route answers is held to the contract itself: the schemas
// of protocol/openapi.yaml, with their required members, enums, id patterns
// and the conditions between a job's status and what it carries.

import fs from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { Ajv2020, type ValidateFunction } from 'ajv/dist/2020.js'
import addFormats from 'ajv-formats'
import yaml from 'js-yaml'

const OPENAPI = path.join(path.dirname(fileURLToPath(import.meta.url)), '..', '..', '..', 'protocol', 'openapi.yaml')

let openapi: { components: { schemas: Record<string, unknown> } } | undefined
let ajv: Ajv2020 | undefined
const compiled = new Map<string, ValidateFunction>()

function validator(name: string): ValidateFunction {
  openapi ??= yaml.load(fs.readFileSync(OPENAPI, 'utf8')) as NonNullable<typeof openapi>
  if (ajv === undefined) addFormats((ajv = new Ajv2020({ strict: false, allErrors: true })))
  if (!Object.hasOwn(openapi.components.schemas, name)) throw new Error(`the contract has no schema ${name}`)
  const validate = compiled.get(name) ?? ajv.compile({ ...openapi, $ref: `#/components/schemas/${name}` })
  compiled.set(name, validate)
  return validate
}

/** An answer that is not what the contract says it is: it is never read further. */
export class ContractError extends Error {}

/** `answer` as the contract's `name`, or a `ContractError` that says where it differs. */
export function conform<T>(name: string, answer: unknown): T {
  const validate = validator(name)
  if (validate(answer)) return answer as T
  const where = (validate.errors ?? []).slice(0, 3).map((e) => `${e.instancePath || '/'} ${e.message ?? ''}`.trim())
  throw new ContractError(`the answer is not a ${name}: ${where.join('; ')}`)
}
