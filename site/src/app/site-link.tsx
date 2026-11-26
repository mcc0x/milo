import type { ComponentPropsWithoutRef } from 'react';

export function SiteLink(props: Omit<ComponentPropsWithoutRef<'a'>, 'target' | 'rel' | 'download'>) {
  return <a {...props} target="_blank" rel="noopener" />;
}
