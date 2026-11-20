import {
  BarChart2,
  LayoutDashboard,
  Settings,
  TrendingUp,
  Wallet,
} from 'lucide-react';
import type { ScreenId } from '../../types';

export function BottomNavBar({
  activeScreen,
  onSelectScreen,
}: {
  activeScreen: ScreenId;
  onSelectScreen: (screen: ScreenId) => void;
}) {
  const itemClass = (active: boolean) =>
    `flex flex-col items-center justify-center min-h-11 min-w-14 py-1.5 px-2 rounded-xl transition-all select-none cursor-pointer ${
      active
        ? 'text-primary font-bold'
        : 'text-on-surface-variant hover:text-on-surface'
    }`;

  return (
    <nav className="md:hidden fixed bottom-0 left-0 right-0 bg-surface/90 backdrop-blur-md border-t border-outline-variant/60 flex items-center justify-around px-2 pt-2 pb-safe z-50 shadow-lg">
      <button
        type="button"
        onClick={() => onSelectScreen('dashboard')}
        aria-current={activeScreen === 'dashboard' ? 'page' : undefined}
        className={itemClass(activeScreen === 'dashboard')}
      >
        <LayoutDashboard size={20} className={activeScreen === 'dashboard' ? 'scale-110 transition-transform' : ''} />
        <span className="text-[10px] mt-1 font-medium">Dashboard</span>
      </button>
      <button
        type="button"
        onClick={() => onSelectScreen('positions')}
        aria-current={activeScreen === 'positions' ? 'page' : undefined}
        className={itemClass(activeScreen === 'positions')}
      >
        <Wallet size={20} className={activeScreen === 'positions' ? 'scale-110 transition-transform' : ''} />
        <span className="text-[10px] mt-1 font-medium">Positions</span>
      </button>
      <button
        type="button"
        onClick={() => onSelectScreen('analytics')}
        aria-current={activeScreen === 'analytics' ? 'page' : undefined}
        className={itemClass(activeScreen === 'analytics')}
      >
        <BarChart2 size={20} className={activeScreen === 'analytics' ? 'scale-110 transition-transform' : ''} />
        <span className="text-[10px] mt-1 font-medium">Analytics</span>
      </button>
      <button
        type="button"
        onClick={() => onSelectScreen('portfolio')}
        aria-current={activeScreen === 'portfolio' ? 'page' : undefined}
        className={itemClass(activeScreen === 'portfolio')}
      >
        <TrendingUp size={20} className={activeScreen === 'portfolio' ? 'scale-110 transition-transform' : ''} />
        <span className="text-[10px] mt-1 font-medium">Portfolio</span>
      </button>
      <button
        type="button"
        onClick={() => onSelectScreen('settings')}
        aria-current={activeScreen === 'settings' ? 'page' : undefined}
        className={itemClass(activeScreen === 'settings')}
      >
        <Settings size={20} className={activeScreen === 'settings' ? 'scale-110 transition-transform' : ''} />
        <span className="text-[10px] mt-1 font-medium">Settings</span>
      </button>
    </nav>
  );
}
