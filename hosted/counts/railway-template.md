# First provisioning on Railway

Counts runs as two new services, `counts` and `counts-postgres`, inside Milo's **existing** `production` environment. Nothing in the repository creates them, and a pull request is not authorization to provision or apply. Complete the [privacy checks](README.md) before allowing client traffic. The `site` and `connect` services, their domains and the `telegram-bindings` volume stay as they are.

## Why a template, then infrastructure as code

The admin username and password must be strong random values that an authorized owner can read back from the service's Variables. Railway generates such values only when a **template is deployed**: its `${{secret(length, alphabet)}}` functions run at that moment and the result is stored as an ordinary service variable ([template variable functions](https://docs.railway.com/templates/create#template-variable-functions)). Infrastructure as code has no documented way to generate a secret; written there, `${{secret(...)}}` would not be a generator, so `.railway/railway.ts` never sets these variables. Project variables stay unmanaged (`variables: { managed: false }`), so applying that file leaves every variable value alone.

So the order is fixed: provision with the template, then adopt the two services into `.railway/railway.ts`, then check that planning changes nothing else.

## 1. Provision (needs the maintainer's explicit approval)

1. In **Workspace → Templates → New Template**, create a private, unpublished template with two services.
   - `counts-postgres`: Railway's PostgreSQL service, in `us-west2`, with its own persistent storage, no domain and no public TCP proxy. Name it exactly this.
   - `counts`: this repository (`mcc0x/milo`, branch `main`, Wait for CI on), root directory `/hosted/counts`, builder Dockerfile (`Dockerfile`), healthcheck `/readyz` (90 seconds), one replica in `us-west2`. Do not attach the website, the settings editor or the pairing relay.
2. In the template composer, set `counts`'s three variables from [railway-template.variables.json](railway-template.variables.json), using its own names. These are expressions, not credentials. **Do not seal them:** a sealed value can never be read back. Add no other variables.
3. Deploy that template **into the Milo project's `production` environment** (Railway's deploy-into-an-existing-project flow, not a new project). Do not publish it, and do not deploy it twice: a second deployment is a second pair of services with different credentials, not a rotation.
4. Check, without printing values, that the owner can see both admin variables under **counts → Variables**, and that `counts-postgres` generated its own password.

Provisioning does not attach a domain. The service has no public address until you add one, and the Postgres service must have none.

## 2. Verify the database stays private

Inspect the template's network settings before provisioning: omit the default Postgres template's TCP proxy if present. After provisioning confirm that `counts-postgres` has no TCP proxy and no domain (Settings → Networking, or the read-only proxy listing for that service). Stop if it does. The receiver reaches the database through `DATABASE_URL`, which uses the private network name.

## 3. Public address

The receiver may use a Railway-generated `*.up.railway.app` domain; no custom domain and no DNS change are part of this. Add it only after the privacy checks pass. Railway's HTTP logs record client details that the receiver cannot turn off (see the README), so that log review decides whether client traffic may be sent.

## 4. Adopt into `.railway/railway.ts`

The checked-in file already declares both services so a whole-project apply cannot omit them. Do not apply the file to bootstrap the services: it does not generate credentials. After template provisioning, follow the [Railway skill](../../skills/milo-railway/SKILL.md): link the project, run `railway config plan --environment production` and review the resource-only diff. The expected plan has **no change to `site`, `connect`, their domains or the relay volume**, and no replacement of the two new services. Stop if the plan proposes a domain removal, a region or volume change, a variable edit, or any unexpected creation/deletion. Never use `--include-variables` or publish exported variables.

## Credentials and rotation

Use the values in the browser's HTTP authentication prompt over HTTPS. Never put them in URLs, shell arguments, logs or screenshots. A fresh browser without them must see nothing. Redeploying reuses the saved variables.

**Rotate explicitly.** In the `counts` service Variables, replace both values with independently generated ones (for example 32 and 64 random lowercase hexadecimal characters from a password manager). Stage both edits together, deploy, wait for old replicas to drain, and check in private browser sessions that the old pair fails and the new pair works. Clearing either variable makes the receiver refuse to start. A forgotten password is recoverable by an authorized owner from Variables; do not reprovision.
