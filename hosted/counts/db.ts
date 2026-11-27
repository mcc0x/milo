import { SQL } from "bun";
import schema from "./schema.sql" with { type: "text" };

export type DayCounts = { day: string; dailyActive: number; newInstalls: number };
export type MonthCounts = { month: string; monthlyActive: number };
export type Counts = { day: string; newInstall: boolean; monthlyActive: boolean };

type Row = { day: string; daily_active: string | number; new_installs: string | number };

const migrationLock = 129_001;

export function openDb(url: string) {
  const sql = new SQL(url, { max: 4, idleTimeout: 30, connectionTimeout: 10 });
  const toCounts = (row: Row): DayCounts => ({
    day: row.day,
    dailyActive: Number(row.daily_active),
    newInstalls: Number(row.new_installs),
  });

  return {
    async migrate() {
      await sql.begin(async (tx) => {
        const [role] = await tx`SELECT has_schema_privilege('public', 'CREATE') AS can_migrate`;
        if (!role.can_migrate) {
          await tx`SELECT day, daily_active, new_installs FROM milo_counts_daily LIMIT 0`;
          await tx`SELECT month, monthly_active FROM milo_counts_monthly LIMIT 0`;
          return;
        }
        await tx`SELECT pg_advisory_xact_lock(${migrationLock})`;
        await tx.unsafe(schema);
      });
    },
    async add({ day, newInstall, monthlyActive }: Counts) {
      await sql.begin(async (tx) => {
        await tx`
          INSERT INTO milo_counts_daily AS c (day, daily_active, new_installs)
          VALUES (${day}::date, 1, ${newInstall ? 1 : 0})
          ON CONFLICT (day) DO UPDATE SET
            daily_active = c.daily_active + 1,
            new_installs = c.new_installs + EXCLUDED.new_installs`;
        if (!monthlyActive) return;
        await tx`
          INSERT INTO milo_counts_monthly AS m (month, monthly_active)
          VALUES (date_trunc('month', ${day}::date)::date, 1)
          ON CONFLICT (month) DO UPDATE SET monthly_active = m.monthly_active + 1`;
      });
    },
    async range(from: string, to: string): Promise<DayCounts[]> {
      const rows: Row[] = await sql`
        SELECT to_char(day, 'YYYY-MM-DD') AS day, daily_active, new_installs
        FROM milo_counts_daily WHERE day BETWEEN ${from}::date AND ${to}::date ORDER BY day`;
      return rows.map(toCounts);
    },
    async months(from: string, to: string): Promise<MonthCounts[]> {
      const rows: { month: string; monthly_active: string | number }[] = await sql`
        SELECT to_char(month, 'YYYY-MM-DD') AS month, monthly_active
        FROM milo_counts_monthly WHERE month BETWEEN ${from}::date AND ${to}::date ORDER BY month`;
      return rows.map((row) => ({ month: row.month, monthlyActive: Number(row.monthly_active) }));
    },
    async totalNewInstalls(): Promise<number> {
      const [row] = await sql`SELECT COALESCE(sum(new_installs), 0)::text AS total FROM milo_counts_daily`;
      return Number(row.total);
    },
    async ping() {
      await sql`SELECT 1`;
    },
    close: () => sql.close(),
  };
}

export type Db = ReturnType<typeof openDb>;
