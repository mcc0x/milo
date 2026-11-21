import { execFileSync } from 'node:child_process';
import Dashboard from '../components/shell';

// Fast Refresh can be paired with an installed backend that predates milo_version.
const devVersion = process.env.NODE_ENV === 'development'
  ? execFileSync(process.env.MILO_BIN || 'milo', ['--version'], { encoding: 'utf8' }).trim().replace(/^milo /, '')
  : undefined;

export default function SettingsPage() {
  return <Dashboard devVersion={devVersion} />;
}
