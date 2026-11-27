\set ON_ERROR_STOP on
BEGIN;
SELECT current_database() AS counts_database \gset
CREATE ROLE milo_counts_receiver NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS;
REVOKE TEMPORARY ON DATABASE :"counts_database" FROM PUBLIC;
REVOKE CREATE ON SCHEMA public FROM PUBLIC;
GRANT CONNECT ON DATABASE :"counts_database" TO milo_counts_receiver;
GRANT USAGE ON SCHEMA public TO milo_counts_receiver;
GRANT SELECT, INSERT, UPDATE (daily_active, new_installs) ON public.milo_counts_daily TO milo_counts_receiver;
GRANT SELECT, INSERT, UPDATE (monthly_active) ON public.milo_counts_monthly TO milo_counts_receiver;
ALTER ROLE milo_counts_receiver IN DATABASE :"counts_database" SET search_path = pg_catalog, public;
ALTER ROLE milo_counts_receiver IN DATABASE :"counts_database" SET log_statement = 'none';
ALTER ROLE milo_counts_receiver IN DATABASE :"counts_database" SET log_duration = off;
ALTER ROLE milo_counts_receiver IN DATABASE :"counts_database" SET log_min_duration_statement = -1;
ALTER ROLE milo_counts_receiver IN DATABASE :"counts_database" SET log_min_duration_sample = -1;
ALTER ROLE milo_counts_receiver IN DATABASE :"counts_database" SET log_transaction_sample_rate = 0;
ALTER ROLE milo_counts_receiver IN DATABASE :"counts_database" SET log_parameter_max_length = 0;
ALTER ROLE milo_counts_receiver IN DATABASE :"counts_database" SET log_parameter_max_length_on_error = 0;
ALTER ROLE milo_counts_receiver IN DATABASE :"counts_database" SET log_min_error_statement = panic;
ALTER ROLE milo_counts_receiver IN DATABASE :"counts_database" SET log_min_messages = fatal;
ALTER ROLE milo_counts_receiver IN DATABASE :"counts_database" SET log_error_verbosity = terse;
COMMIT;
