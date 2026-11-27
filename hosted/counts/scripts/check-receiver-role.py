"""Run only inside scripts/pg-ephemeral.sh, against its loopback database and private log."""
import base64
import contextlib
import datetime
import http.client
import json
import os
from pathlib import Path
import secrets
import socket
import subprocess
import tempfile
import time
from urllib.parse import urlsplit, urlunsplit


root = Path(__file__).resolve().parents[1]
owner_url = os.environ["MILO_COUNTS_TEST_DATABASE_URL"]
owner = urlsplit(owner_url)
log = Path(os.environ["MILO_COUNTS_TEST_POSTGRES_LOG"])
if (owner.hostname != "127.0.0.1" or owner.username != "milo" or owner.path != "/milo_counts"
        or log.name != "postgres.log" or not log.parent.name.startswith("milo-counts-pg.")):
    raise SystemExit("Run inside scripts/pg-ephemeral.sh.")
role = "milo_counts_receiver"
password = secrets.token_hex(32)
runtime_url = urlunsplit(owner._replace(netloc=f"{role}:{password}@127.0.0.1:{owner.port}"))
admin_user, admin_password = secrets.token_hex(16), secrets.token_hex(32)
auth = "Basic " + base64.b64encode(f"{admin_user}:{admin_password}".encode()).decode()


def sql(statement, runtime=False, extra=None):
    env = dict(os.environ, PGUSER=role if runtime else owner.username,
               PGPASSWORD=password if runtime else owner.password, PGDATABASE=owner.path[1:],
               PGHOST=owner.hostname, PGPORT=str(owner.port), **(extra or {}))
    result = subprocess.run([str(Path(os.environ.get("PG_BIN", "")) / "psql"), "-XqAtw", "-v", "ON_ERROR_STOP=1"],
                            input=statement, text=True, env=env, capture_output=True)
    if result.returncode:
        raise AssertionError("Fixture SQL failed")
    return result.stdout.strip()


@contextlib.contextmanager
def running(url):
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
    env = dict(os.environ, DATABASE_URL=url, PORT=str(port), MILO_COUNTS_HOST="127.0.0.1",
               MILO_COUNTS_ADMIN_USER=admin_user, MILO_COUNTS_ADMIN_PASSWORD=admin_password)
    env.pop("MILO_COUNTS_TEST_DATABASE_URL")
    with tempfile.TemporaryFile() as output:
        proc = subprocess.Popen(["bun", "run", "--no-env-file", "server.ts"], cwd=root,
                                env=env, stdout=output, stderr=subprocess.STDOUT)

        def request(method, path, body=None, headers=None):
            with contextlib.closing(http.client.HTTPConnection("127.0.0.1", port, timeout=3)) as conn:
                conn.request(method, path, body=body, headers=headers or {})
                response = conn.getresponse()
                return response.status, response.read()

        try:
            deadline = time.monotonic() + 10
            while True:
                if proc.poll() is not None or time.monotonic() > deadline:
                    raise AssertionError("Receiver did not start")
                try:
                    if request("GET", "/readyz")[0] == 200:
                        break
                except ConnectionRefusedError:
                    pass
                time.sleep(0.05)
            yield request
        finally:
            proc.terminate()
            proc.wait(timeout=10)
            output.seek(0)
            assert output.read().decode().strip() == f"milo counts listening on port {port}"


def send(request):
    payload = {"day": str(datetime.datetime.now(datetime.timezone.utc).date()), "new_install": True, "monthly_active": True}
    return request("POST", "/v1/counts", json.dumps(payload), {"content-type": "application/json"})


def failed_write(request):
    offset = log.stat().st_size
    assert send(request) == (503, b"Unavailable.\n")
    assert sql("SELECT count(*) FROM milo_counts_daily; SELECT count(*) FROM milo_counts_monthly;") == "0\n0"
    with log.open("rb") as stream:
        stream.seek(offset)
        return stream.read()


with running(owner_url) as request:
    sql("ALTER TABLE milo_counts_monthly ADD CONSTRAINT synthetic_reject_month CHECK (monthly_active < 1);")
    baseline = failed_write(request)
    assert b"ERROR:" in baseline and b"Failing row contains" in baseline and b"STATEMENT:" in baseline
    assert b"INSERT INTO milo_counts_monthly" in baseline and b"synthetic_reject_month" in baseline
print("Baseline: the failed monthly upsert rolled back, but PostgreSQL emitted the SQL and failing row.")

sql((root / "scripts/receiver-role.sql").read_text())
sql("\\getenv password MILO_FIXTURE_PASSWORD\nALTER ROLE milo_counts_receiver LOGIN PASSWORD :'password';",
    extra={"MILO_FIXTURE_PASSWORD": password})
assert sql("""SELECT NOT rolsuper AND NOT rolcreatedb AND NOT rolcreaterole AND NOT rolreplication
    AND NOT rolbypassrls AND NOT has_database_privilege(current_database(),'TEMP')
    AND NOT has_schema_privilege('public','CREATE')
    AND NOT has_table_privilege('milo_counts_daily','TRUNCATE')
    AND NOT has_table_privilege('milo_counts_monthly','DELETE')
    AND NOT has_column_privilege('milo_counts_daily','day','UPDATE')
    FROM pg_roles WHERE rolname=current_user;""", runtime=True) == "t"
with running(runtime_url) as request:
    assert not failed_write(request), "Restricted write emitted a PostgreSQL log record"
    sql("ALTER TABLE milo_counts_monthly DROP CONSTRAINT synthetic_reject_month;")
    assert send(request) == (204, b"")
    assert sql("SELECT daily_active,new_installs FROM milo_counts_daily; SELECT monthly_active FROM milo_counts_monthly;") == "1|1\n1"
    status, page = request("GET", "/", headers={"Authorization": auth})
    assert status == 200 and b"1 since counting began" in page
assert sql("SHOW log_min_error_statement;") == "error"
print("Restricted login: same 503 and rollback, no PostgreSQL failure output; readiness, successful write and authenticated dashboard passed. Owner error logging unchanged.")
