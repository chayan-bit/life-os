import React, { useState, useEffect } from 'react';
import { Link, useLocation } from 'react-router-dom';
import BrandMark from './BrandMark';
import AIConsole from './AIConsole';
import CommandBar from './CommandBar';
import { ensureBaseline } from '../lib/vcs';
import { apiCall } from '../lib/api';
import { hydrateFromStorage } from '../lib/moduleRegistry';
import { useModuleStream } from '../lib/useModuleStream';
import {
  Brain,
  LayoutDashboard,
  Database,
  Cpu,
  Settings,
  Menu,
  X,
  Terminal,
  FileText,
  LogOut,
  Moon,
  Sun,
  FolderGit2,
  Boxes,
  Compass,
  PanelLeftClose,
  PanelLeftOpen,
  GitBranch,
  Sparkles,
  History,
  Gauge,
  Store,
  Mail,
  Calendar,
  FolderOpen,
  StickyNote,
  MessageSquare,
  BookOpen,
  Plane,
  Inbox,
  Activity
} from 'lucide-react';

export default function Layout({ children, onLogout }) {
  const location = useLocation();
  const [isMobileMenuOpen, setIsMobileMenuOpen] = useState(false);
  const [isDarkMode, setIsDarkMode] = useState(false);
  const [isSidebarCollapsed, setIsSidebarCollapsed] = useState(
    localStorage.getItem('life_os_sidebar_collapsed') === 'true'
  );
  const [apiOnline, setApiOnline] = useState(null); // null = checking, true/false once known
  const [installedModules, setInstalledModules] = useState(() => hydrateFromStorage());

  // Live module hot-reload: subscribes to /api/stream/modules (SSE, polling
  // fallback) and re-renders the nav the instant a new module installs.
  useModuleStream();
  useEffect(() => {
    const onMounted = (e) => {
      setInstalledModules((prev) => {
        if (prev.some((m) => m.id === e.detail.id)) return prev;
        return [...prev, e.detail.manifest];
      });
    };
    window.addEventListener('lifeos:module-mounted', onMounted);
    return () => window.removeEventListener('lifeos:module-mounted', onMounted);
  }, []);

  useEffect(() => {
    const savedTheme = localStorage.getItem('life_os_theme');
    if (savedTheme === 'dark') {
      setIsDarkMode(true);
      document.documentElement.classList.add('dark');
    }
    // Seal the current state as the protected baseline commit on first run.
    ensureBaseline();
  }, []);

  // Global API reachability indicator - polled, not just checked once, so
  // killing the backend mid-session flips the pill/banner without a reload.
  useEffect(() => {
    let cancelled = false;
    const checkHealth = async () => {
      const { ok, offline } = await apiCall('GET', '/api/health');
      if (!cancelled) setApiOnline(ok && !offline);
    };
    checkHealth();
    const interval = setInterval(checkHealth, 15000);
    return () => { cancelled = true; clearInterval(interval); };
  }, []);

  const toggleSidebar = () => {
    setIsSidebarCollapsed((prev) => {
      const next = !prev;
      localStorage.setItem('life_os_sidebar_collapsed', String(next));
      return next;
    });
  };

  const toggleTheme = () => {
    if (isDarkMode) {
      document.documentElement.classList.remove('dark');
      localStorage.setItem('life_os_theme', 'light');
      setIsDarkMode(false);
    } else {
      document.documentElement.classList.add('dark');
      localStorage.setItem('life_os_theme', 'dark');
      setIsDarkMode(true);
    }
  };

  // Grouped, compressed IA. Self-Extension + Agent Harness + Harness Loop are
  // merged into Harness; Repository + VCS/Media Ingest into Storage.
  const navGroups = [
    {
      group: 'Workspace',
      items: [
        { name: 'Dashboard', href: '/dashboard', icon: LayoutDashboard },
        { name: 'Knowledge', href: '/knowledge', icon: Compass },
        { name: 'Learning', href: '/m/learning', icon: Sparkles },
        { name: 'Coding', href: '/m/coding', icon: FolderGit2 },
        { name: 'Trading', href: '/m/trading', icon: Gauge },
        { name: 'Social', href: '/m/social', icon: Sparkles },
        { name: 'Marketing', href: '/m/marketing', icon: LayoutDashboard },
        { name: 'Design', href: '/m/design', icon: Boxes },
        { name: 'Email', href: '/m/email', icon: Mail },
        { name: 'Calendar', href: '/m/calendar', icon: Calendar },
        { name: 'Files', href: '/m/files', icon: FolderOpen },
        { name: 'Notion', href: '/m/notion', icon: StickyNote },
        { name: 'Slack', href: '/m/slack', icon: MessageSquare },
        { name: 'Reading', href: '/m/reading', icon: BookOpen },
        { name: 'Travel', href: '/m/travel', icon: Plane },
        { name: 'Modules', href: '/modules', icon: Cpu },
        { name: 'Database', href: '/database', icon: Database },
        { name: 'Graph', href: '/graph', icon: GitBranch },
        { name: 'Dashboards', href: '/dashboards', icon: LayoutDashboard },
      ],
    },
    {
      group: 'Build',
      items: [
        { name: 'Harness', href: '/harness', icon: Boxes },
        { name: 'Storage & VCS', href: '/storage', icon: FolderGit2 },
        { name: 'Marketplace', href: '/marketplace', icon: Store },
        { name: 'Refine Demo', href: '/refine-demo', icon: Boxes },
      ],
    },
    {
      group: 'System',
      items: [
        { name: 'Integrations', href: '/integrations', icon: Settings },
        { name: 'Docs', href: '/docs', icon: FileText },
        { name: 'Approvals', href: '/approvals', icon: Inbox },
        { name: 'Agent Ledger', href: '/agent-ledger', icon: History },
        { name: 'Memory', href: '/memory', icon: Brain },
        { name: 'Observe', href: '/observe', icon: Activity },
      ],
    },
    // Hot-installed modules (issue #29): appear here the instant the SSE
    // stream (or its polling fallback) reports a module.installed event -
    // no manual refresh needed.
    ...(installedModules.length
      ? [{
          group: 'Installed',
          items: installedModules.map((m) => ({ name: m.name || m.id, href: `/m/${m.id}`, icon: Sparkles })),
        }]
      : []),
  ];
  const navigation = navGroups.flatMap((g) => g.items);

  const isActive = (path) => location.pathname === path;
  const activeLabel = navigation.find((item) => isActive(item.href))?.name || 'Overview';

  return (
    <div className="min-h-screen neo-bg text-neo-text flex">
      {/* Mobile Drawer Menu */}
      {isMobileMenuOpen && (
        <div className="fixed inset-0 z-50 md:hidden">
          <button
            type="button"
            className="absolute inset-0 w-full h-full bg-[var(--neo-border)]/40"
            aria-label="Close navigation menu"
            onClick={() => setIsMobileMenuOpen(false)}
          />
          <aside className="absolute left-0 top-0 h-full w-[280px] neo-surface neo-border-thick shadow-[4px_0px_0px_0px_#1c1c0f] flex flex-col">
            <div className="px-6 py-6 neo-divider flex flex-col items-center gap-3 text-center">
              <BrandMark className="w-20 h-20" />
              <div>
                <div className="neo-title-md leading-none">Life OS</div>
                <div className="neo-label-sm text-neo-text-muted mt-1">Agentic Personal OS</div>
              </div>
            </div>

            <nav className="flex-1 px-4 py-4 space-y-4 overflow-y-auto">
              {navGroups.map((g) => (
                <div key={g.group} className="space-y-2">
                  <div className="neo-label-sm text-neo-text-muted text-[10px] px-1">{g.group}</div>
                  {g.items.map((item) => (
                    <Link
                      key={item.name}
                      to={item.href}
                      onClick={() => setIsMobileMenuOpen(false)}
                      className={`flex items-center gap-3 px-4 py-3 neo-label-md transition-all border-2 ${
                        isActive(item.href)
                          ? 'bg-neo-blue text-white border-neo-border neo-shadow'
                          : 'text-neo-text border-transparent hover:bg-neo-surface-high hover:border-neo-border neo-shadow-hover'
                      }`}
                    >
                      <item.icon size={20} />
                      {item.name}
                    </Link>
                  ))}
                </div>
              ))}
            </nav>

            <div className="px-4 py-6 mt-auto border-t-2 border-neo-border flex flex-col gap-2">
              <div className="text-center neo-label-sm text-neo-text-muted">
                Local Trusted Host (Mac OS)
              </div>
              <div className="flex items-center gap-2 justify-center neo-tag">
                <Terminal size={14} />
                <span>ONLINE: 127.0.0.1</span>
              </div>
            </div>
          </aside>
        </div>
      )}

      {/* Desktop Sidebar */}
      <aside
        className={`hidden md:flex md:flex-col neo-surface neo-border-thick border-r-4 border-neo-border shadow-[4px_0px_0px_0px_#1c1c0f] transition-[width] duration-200 ${
          isSidebarCollapsed ? 'w-16' : 'w-52'
        }`}
      >
        <div className={`px-3 py-3 neo-divider flex items-center gap-3 ${isSidebarCollapsed ? 'justify-center' : ''}`}>
          <BrandMark className="w-9 h-9 shrink-0" />
          {!isSidebarCollapsed && (
            <div>
              <div className="neo-title-md text-sm leading-none">Life OS</div>
              <div className="neo-label-sm text-neo-text-muted text-[9px] mt-0.5">Agentic Personal OS</div>
            </div>
          )}
        </div>

        <button
          onClick={toggleSidebar}
          className={`mx-2 mt-2 neo-icon-btn neo-radius-none p-1.5 flex items-center justify-center ${
            isSidebarCollapsed ? '' : 'self-end'
          }`}
          aria-label={isSidebarCollapsed ? 'Expand sidebar' : 'Collapse sidebar'}
          title={isSidebarCollapsed ? 'Expand sidebar' : 'Collapse sidebar'}
        >
          {isSidebarCollapsed ? <PanelLeftOpen size={16} /> : <PanelLeftClose size={16} />}
        </button>

        <nav className="flex-1 px-2 py-2 flex flex-col gap-2 overflow-y-auto">
          {navGroups.map((g) => (
            <div key={g.group} className="flex flex-col gap-0.5">
              {!isSidebarCollapsed && (
                <div className="neo-label-sm text-neo-text-muted text-[9px] px-2 pt-1 pb-0.5">{g.group}</div>
              )}
              {g.items.map((item) => (
                <Link
                  key={item.name}
                  to={item.href}
                  title={isSidebarCollapsed ? item.name : undefined}
                  className={`flex items-center gap-2 px-3 py-2 neo-label-sm text-[11px] transition-all border-2 ${
                    isSidebarCollapsed ? 'justify-center' : ''
                  } ${
                    isActive(item.href)
                      ? 'bg-neo-blue text-white border-neo-border neo-shadow'
                      : 'text-neo-text border-transparent hover:bg-neo-surface-high hover:border-neo-border'
                  }`}
                >
                  <item.icon size={14} className="shrink-0" />
                  {!isSidebarCollapsed && <span className="truncate">{item.name}</span>}
                </Link>
              ))}
            </div>
          ))}
        </nav>

        {!isSidebarCollapsed && (
          <div className="px-3 py-3 mt-auto border-t-2 border-neo-border flex flex-col gap-1.5">
            <div className="neo-label-sm text-neo-text-muted text-[9px]">
              Local Trusted Host (Mac OS)
            </div>
            <div className="flex items-center gap-1.5 neo-tag text-[9px] px-2 py-1">
              <Terminal size={10} />
              <span>ONLINE: 127.0.0.1</span>
            </div>
          </div>
        )}
      </aside>

      {/* Main Content Area */}
      <main className="flex-1 flex flex-col min-w-0 overflow-hidden">
        <header className="sticky top-0 z-40 neo-surface neo-divider shadow-[0px_4px_0px_0px_#1c1c0f] px-6 py-4 flex items-center justify-between">
          <div className="flex items-center gap-3">
            <button
              onClick={() => setIsMobileMenuOpen(!isMobileMenuOpen)}
              className="md:hidden neo-icon-btn neo-radius-none p-2"
              aria-label="Toggle menu"
            >
              {isMobileMenuOpen ? <X size={20} /> : <Menu size={20} />}
            </button>
            <h1 className="neo-title-md hidden md:block">{activeLabel}</h1>
            <span className="neo-tag bg-neo-yellow hidden lg:inline-flex">SAAS-READY</span>
          </div>

          <div className="flex items-center gap-3">
            <div
              className={`flex items-center gap-2 px-3 py-1.5 neo-border neo-shadow-sm neo-label-sm ${
                apiOnline === false ? 'bg-neo-red text-white' : 'bg-neo-mint'
              }`}
              title={apiOnline === false ? 'lifeos-api unreachable at ' + (import.meta.env?.VITE_API_URL || 'http://127.0.0.1:8080') : 'lifeos-api reachable'}
            >
              <div
                className={`w-2.5 h-2.5 rounded-full ${
                  apiOnline === null ? 'bg-neo-text-muted animate-pulse'
                  : apiOnline ? 'bg-[var(--neo-text)] animate-pulse'
                  : 'bg-white'
                }`}
              />
              <span>{apiOnline === null ? 'Checking API…' : apiOnline ? 'API online' : 'API offline'}</span>
            </div>
            
            <button
              onClick={toggleTheme}
              className="neo-btn p-2 bg-neo-surface-high hover:bg-neo-yellow flex items-center justify-center transition-colors"
              title="Toggle Theme"
            >
              {isDarkMode ? <Sun size={18} /> : <Moon size={18} />}
            </button>
            
            <button
              onClick={onLogout}
              className="neo-btn p-2 bg-neo-red text-white hover:bg-neo-red/90 flex items-center gap-1 text-xs font-mono font-bold"
              title="Logout session"
            >
              <LogOut size={16} />
              <span className="hidden sm:inline">LOGOUT</span>
            </button>
            
            <Link
              to="/profile"
              title="Profile & account"
              className="w-10 h-10 neo-border neo-shadow bg-neo-yellow flex items-center justify-center font-bold text-neo-text hover:bg-neo-mint transition-colors"
            >
              {(localStorage.getItem('life_os_user_name') || 'LO')
                .split(' ')
                .map((p) => p[0])
                .join('')
                .slice(0, 2)
                .toUpperCase()}
            </Link>
          </div>
        </header>

        {apiOnline === false && (
          <div className="px-6 py-2 bg-neo-red text-white text-xs font-mono font-bold text-center neo-divider">
            lifeos-api is unreachable - reads/writes are falling back to local mock data where supported.
          </div>
        )}

        <div className="flex-1 overflow-y-auto p-6 md:p-8">
          {children}
        </div>
      </main>

      {/* App-wide AI surface - reachable from every page */}
      <AIConsole />
      <CommandBar />
    </div>
  );
}
