'use client';

import { useEffect } from 'react';
import { mountSite } from '../main';

/** Starts the demo and the page's small interactions once the markup is in the document. */
export function Enhance() {
  useEffect(mountSite, []);
  return null;
}
