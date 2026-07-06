import React, { useState, Suspense, lazy } from 'react';
import { BrowserRouter, Routes, Route, Navigate } from 'react-router-dom';
import Layout from './components/Layout';
import LoginPage from './pages/LoginPage';
import { apiCall, WORKSPACE_ID_KEY, KEY_TOKEN_KEY, REFRESH_TOKEN_KEY } from './lib/api';

const Dashboard = lazy(() => import('./pages/Dashboard'));
const Database = lazy(() => import('./pages/Database'));
const Modules = lazy(() => import('./pages/Modules'));
const Knowledge = lazy(() => import('./pages/Knowledge'));
const Harness = lazy(() => import('./pages/Harness'));
const Storage = lazy(() => import('./pages/Storage'));
const Integrations = lazy(() => import('./pages/Integrations'));
const DocsHub = lazy(() => import('./pages/DocsHub'));
const Profile = lazy(() => import('./pages/Profile'));
const RefineDemo = lazy(() => import('./pages/RefineDemo'));
const GraphView = lazy(() => import('./pages/GraphView'));
const InstalledModulePage = lazy(() => import('./pages/InstalledModulePage'));
const ModuleDashboards = lazy(() => import('./pages/ModuleDashboards'));
const AgentLedger = lazy(() => import('./pages/AgentLedger'));
const ApprovalInbox = lazy(() => import('./pages/ApprovalInbox'));
const MemoryInspector = lazy(() => import('./pages/MemoryInspector'));
const Marketplace = lazy(() => import('./pages/Marketplace'));

const PageFallback = () => (
  <div style={{ padding: '2rem', textAlign: 'center', color: '#888' }}>Loading...</div>
);

export default function App() {
  const [isLoggedIn, setIsLoggedIn] = useState(
    localStorage.getItem('life_os_loggedin') === 'true'
  );

  const handleLogout = () => {
    // Real session revocation (issue #100): best-effort - if the API is
    // unreachable the local session is cleared anyway, matching the
    // frontend's general fail-open-to-offline posture.
    const refreshToken = localStorage.getItem(REFRESH_TOKEN_KEY);
    if (refreshToken) {
      apiCall('POST', '/api/logout', { refresh_token: refreshToken });
    }
    localStorage.removeItem('life_os_loggedin');
    localStorage.removeItem(WORKSPACE_ID_KEY);
    localStorage.removeItem(KEY_TOKEN_KEY);
    localStorage.removeItem(REFRESH_TOKEN_KEY);
    setIsLoggedIn(false);
  };

  if (!isLoggedIn) {
    return <LoginPage onLogin={() => setIsLoggedIn(true)} />;
  }

  return (
    <BrowserRouter>
      <Suspense fallback={<PageFallback />}>
      <Routes>
        <Route
          path="/dashboard"
          element={
            <Layout onLogout={handleLogout}>
              <Dashboard />
            </Layout>
          }
        />
        <Route
          path="/database"
          element={
            <Layout onLogout={handleLogout}>
              <Database />
            </Layout>
          }
        />
        <Route
          path="/modules"
          element={
            <Layout onLogout={handleLogout}>
              <Modules />
            </Layout>
          }
        />
        <Route
          path="/knowledge"
          element={
            <Layout onLogout={handleLogout}>
              <Knowledge />
            </Layout>
          }
        />
        <Route
          path="/harness"
          element={
            <Layout onLogout={handleLogout}>
              <Harness />
            </Layout>
          }
        />
        <Route
          path="/storage"
          element={
            <Layout onLogout={handleLogout}>
              <Storage />
            </Layout>
          }
        />
        <Route
          path="/integrations"
          element={
            <Layout onLogout={handleLogout}>
              <Integrations />
            </Layout>
          }
        />
        <Route
          path="/docs"
          element={
            <Layout onLogout={handleLogout}>
              <DocsHub />
            </Layout>
          }
        />
        <Route
          path="/dashboards"
          element={
            <Layout onLogout={handleLogout}>
              <ModuleDashboards />
            </Layout>
          }
        />
        <Route
          path="/m/:id"
          element={
            <Layout onLogout={handleLogout}>
              <InstalledModulePage />
            </Layout>
          }
        />
        <Route
          path="/graph"
          element={
            <Layout onLogout={handleLogout}>
              <GraphView />
            </Layout>
          }
        />
        <Route
          path="/refine-demo"
          element={
            <Layout onLogout={handleLogout}>
              <RefineDemo />
            </Layout>
          }
        />
        <Route
          path="/profile"
          element={
            <Layout onLogout={handleLogout}>
              <Profile />
            </Layout>
          }
        />
        <Route
          path="/agent-ledger"
          element={
            <Layout onLogout={handleLogout}>
              <AgentLedger />
            </Layout>
          }
        />
        <Route
          path="/approvals"
          element={
            <Layout onLogout={handleLogout}>
              <ApprovalInbox />
            </Layout>
          }
        />
        <Route
          path="/marketplace"
          element={
            <Layout onLogout={handleLogout}>
              <Marketplace />
            </Layout>
          }
        />
        <Route
          path="/memory"
          element={
            <Layout onLogout={handleLogout}>
              <MemoryInspector />
            </Layout>
          }
        />
        {/* Back-compat redirects for the pre-merge IA */}
        <Route path="/self-extension" element={<Navigate to="/harness" replace />} />
        <Route path="/agent-harness" element={<Navigate to="/harness" replace />} />
        <Route path="/harness-loop" element={<Navigate to="/harness" replace />} />
        <Route path="/repository" element={<Navigate to="/storage" replace />} />
        <Route path="/vcs-ingest" element={<Navigate to="/storage" replace />} />
        <Route path="/" element={<Navigate to="/dashboard" replace />} />
        <Route path="*" element={<Navigate to="/dashboard" replace />} />
      </Routes>
      </Suspense>
    </BrowserRouter>
  );
}
