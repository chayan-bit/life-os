import React, { useState, useEffect } from 'react';
import { ShieldAlert, KeyRound, User, Briefcase, Sparkles, CheckCircle, Moon, Sun } from 'lucide-react';
import BrandMark from '../components/BrandMark';
import { apiCall, WORKSPACE_ID_KEY, KEY_TOKEN_KEY, REFRESH_TOKEN_KEY } from '../lib/api';

export default function LoginPage({ onLogin }) {
  const [isLogin, setIsLogin] = useState(true);
  const [email, setEmail] = useState('');
  const [password, setPassword] = useState('');

  // Registration States
  const [regName, setRegName] = useState('');
  const [regEmail, setRegEmail] = useState('');
  const [regPassword, setRegPassword] = useState('');
  const [regWorkspace, setRegWorkspace] = useState('');
  const [regSuccessData, setRegSuccessData] = useState(null);

  const [error, setError] = useState('');
  const [isDarkMode, setIsDarkMode] = useState(false);

  useEffect(() => {
    const savedTheme = localStorage.getItem('life_os_theme');
    if (savedTheme === 'dark') {
      setIsDarkMode(true);
      document.documentElement.classList.add('dark');
    }
  }, []);

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

  const handleLoginSubmit = (e) => {
    e.preventDefault();
    setError('');

    // Real login (issue #100): the backend verifies the password against
    // users.password_hash and mints a real access + refresh token pair.
    // No client-side credential storage/matching exists anymore.
    apiCall('POST', '/api/login', { email, password }).then(({ ok, data, error, offline }) => {
      if (offline) {
        setError('Cannot reach the Life OS API. Check that lifeos-api is running.');
        return;
      }
      if (!ok || !data || !data.key_token) {
        setError(error || 'Invalid email or password.');
        return;
      }
      localStorage.setItem('life_os_loggedin', 'true');
      localStorage.setItem('life_os_user_email', email);
      localStorage.setItem(WORKSPACE_ID_KEY, data.workspace_id);
      localStorage.setItem(KEY_TOKEN_KEY, data.key_token);
      localStorage.setItem(REFRESH_TOKEN_KEY, data.refresh_token);
      onLogin();
    });
  };

  const handleRegisterSubmit = (e) => {
    e.preventDefault();
    setError('');

    const payload = {
      email: regEmail,
      name: regName,
      password: regPassword,
      workspace_name: regWorkspace,
    };

    // Real registration (issue #100): the backend hashes and stores the
    // password, and mints an access + refresh token pair. A duplicate
    // email is now a real rejection (log in instead), not a silent
    // re-issue - surface that error rather than hiding it.
    apiCall('POST', '/api/register', payload).then(({ ok, data, error, offline }) => {
      if (offline) {
        setError('Cannot reach the Life OS API. Check that lifeos-api is running.');
        return;
      }
      if (!ok || !data || !data.workspace_id || !data.key_token) {
        setError(error || 'Registration failed. Please check your details and try again.');
        return;
      }

      // Match the login success handler exactly (issue: registration never
      // set this flag, so a fresh registration bounced back to the login
      // screen on first reload even though a valid session was minted).
      localStorage.setItem('life_os_loggedin', 'true');
      localStorage.setItem('life_os_user_email', regEmail);
      localStorage.setItem(WORKSPACE_ID_KEY, data.workspace_id);
      localStorage.setItem(KEY_TOKEN_KEY, data.key_token);
      localStorage.setItem(REFRESH_TOKEN_KEY, data.refresh_token);
      setRegSuccessData({
        email: regEmail,
        name: regName,
        workspace_name: regWorkspace,
        workspace_id: data.workspace_id,
      });
    });
  };

  // Registration already minted and stored a valid session (key_token +
  // refresh_token) - no second login round-trip needed, just enter.
  const proceedToLogin = () => {
    onLogin();
  };

  return (
    <div className="min-h-screen neo-bg flex items-center justify-center p-6">
      <div className="w-full max-w-[480px] neo-surface neo-border-thick neo-shadow bg-neo-surface p-8 flex flex-col gap-6">
        {/* Brand Header */}
        <div className="flex flex-col items-center gap-3 text-center border-b-4 border-neo-border pb-5 relative">
          <button
            onClick={toggleTheme}
            className="absolute top-0 right-0 neo-btn p-2 bg-neo-surface-high hover:bg-neo-yellow flex items-center justify-center transition-colors"
            title="Toggle Theme"
          >
            {isDarkMode ? <Sun size={18} /> : <Moon size={18} />}
          </button>
          <BrandMark className="w-20 h-20" />
          <div>
            <h1 className="neo-title-lg text-neo-text leading-none">Life OS</h1>
            <span className="neo-label-sm text-neo-text-muted mt-1 block">Unified Agentic Terminal</span>
          </div>
        </div>

        {/* Tab Toggle */}
        {!regSuccessData && (
          <div className="flex border-4 border-neo-border neo-shadow-sm">
            <button
              onClick={() => { setIsLogin(true); setError(''); }}
              className={`flex-1 py-2 font-mono text-xs font-bold uppercase transition-colors ${
                isLogin ? 'bg-neo-yellow text-black' : 'bg-neo-surface'
              }`}
            >
              Sign In
            </button>
            <button
              onClick={() => { setIsLogin(false); setError(''); }}
              className={`flex-1 py-2 font-mono text-xs font-bold uppercase border-l-4 border-neo-border transition-colors ${
                !isLogin ? 'bg-neo-yellow text-black' : 'bg-neo-surface'
              }`}
            >
              Register Workspace
            </button>
          </div>
        )}

        {/* SUCCESS VIEW AFTER REGISTER */}
        {regSuccessData ? (
          <div className="flex flex-col gap-5">
            <div className="p-3 bg-neo-mint border-2 border-neo-border flex items-center gap-2 neo-label-sm text-black">
              <CheckCircle size={16} />
              <span className="font-bold">Workspace Scaffolded Successfully!</span>
            </div>

            <p className="text-xs font-semibold leading-relaxed text-neo-text-muted">
              Your tenant workspace is live. Remember the password you set - it's hashed server-side and never stored anywhere in plaintext, including here.
            </p>

            <div className="p-4 bg-neo-bg neo-border font-mono text-xs flex flex-col gap-2">
              <div>
                <span className="text-neo-text-muted font-bold block">WORKSPACE NAME:</span>
                <span className="font-bold text-neo-text">{regSuccessData.workspace_name}</span>
              </div>
              <div className="mt-2">
                <span className="text-neo-text-muted font-bold block">TENANT ID:</span>
                <span className="font-mono text-neo-blue font-bold">{regSuccessData.workspace_id}</span>
              </div>
            </div>

            <button
              onClick={proceedToLogin}
              className="neo-btn w-full py-3 neo-border neo-shadow bg-neo-mint text-black font-bold uppercase transition-all"
            >
              Enter Workspace →
            </button>
          </div>
        ) : isLogin ? (
          /* LOGIN FORM */
          <form onSubmit={handleLoginSubmit} className="flex flex-col gap-5">
            {error && (
              <div className="p-3 bg-neo-red text-white border-2 border-neo-border flex items-center gap-2 neo-label-sm">
                <ShieldAlert size={16} />
                <span>{error}</span>
              </div>
            )}

            <div className="flex flex-col gap-2">
              <label className="neo-label-md flex items-center gap-1.5" htmlFor="email">
                <User size={16} />
                <span>Identity Email</span>
              </label>
              <input
                id="email"
                type="email"
                value={email}
                onChange={(e) => setEmail(e.target.value)}
                className="p-3 neo-border bg-neo-surface text-neo-text font-semibold focus:outline-none focus:bg-neo-bg font-mono text-sm"
                required
              />
            </div>

            <div className="flex flex-col gap-2">
              <label className="neo-label-md flex items-center gap-1.5" htmlFor="password">
                <KeyRound size={16} />
                <span>Password</span>
              </label>
              <input
                id="password"
                type="password"
                value={password}
                onChange={(e) => setPassword(e.target.value)}
                className="p-3 neo-border bg-neo-surface text-neo-text font-semibold focus:outline-none focus:bg-neo-bg font-mono text-sm"
                required
              />
            </div>

            <button
              type="submit"
              className="neo-btn w-full py-4 neo-border neo-shadow bg-neo-mint text-black font-bold uppercase transition-all"
            >
              Authenticate & Enter →
            </button>
          </form>
        ) : (
          /* REGISTRATION / CREATION FORM */
          <form onSubmit={handleRegisterSubmit} className="flex flex-col gap-4">
            {error && (
              <div className="p-3 bg-neo-red text-white border-2 border-neo-border flex items-center gap-2 neo-label-sm">
                <ShieldAlert size={16} />
                <span>{error}</span>
              </div>
            )}

            <div className="flex flex-col gap-1.5">
              <label className="neo-label-md flex items-center gap-1.5" htmlFor="regName">
                <User size={14} />
                <span>Full Name</span>
              </label>
              <input
                id="regName"
                type="text"
                value={regName}
                onChange={(e) => setRegName(e.target.value)}
                placeholder="e.g. Chayan Aggarwal"
                className="p-2.5 neo-border bg-neo-surface text-xs font-semibold focus:outline-none"
                required
              />
            </div>

            <div className="flex flex-col gap-1.5">
              <label className="neo-label-md flex items-center gap-1.5" htmlFor="regEmail">
                <Sparkles size={14} />
                <span>Email Address</span>
              </label>
              <input
                id="regEmail"
                type="email"
                value={regEmail}
                onChange={(e) => setRegEmail(e.target.value)}
                placeholder="e.g. chayan@example.com"
                className="p-2.5 neo-border bg-neo-surface text-xs font-mono focus:outline-none"
                required
              />
            </div>

            <div className="flex flex-col gap-1.5">
              <label className="neo-label-md flex items-center gap-1.5" htmlFor="regPassword">
                <KeyRound size={14} />
                <span>Password</span>
              </label>
              <input
                id="regPassword"
                type="password"
                value={regPassword}
                onChange={(e) => setRegPassword(e.target.value)}
                placeholder="At least 8 characters"
                minLength={8}
                className="p-2.5 neo-border bg-neo-surface text-xs font-semibold focus:outline-none"
                required
              />
            </div>

            <div className="flex flex-col gap-1.5">
              <label className="neo-label-md flex items-center gap-1.5" htmlFor="regWorkspace">
                <Briefcase size={14} />
                <span>Workspace Name</span>
              </label>
              <input
                id="regWorkspace"
                type="text"
                value={regWorkspace}
                onChange={(e) => setRegWorkspace(e.target.value)}
                placeholder="e.g. Chayan's Second Brain"
                className="p-2.5 neo-border bg-neo-surface text-xs font-semibold focus:outline-none"
                required
              />
            </div>

            <button
              type="submit"
              className="neo-btn w-full py-4 neo-border neo-shadow bg-neo-mint text-black font-bold uppercase transition-all mt-2"
            >
              Scaffold Tenant Workspace →
            </button>
          </form>
        )}

        {/* Footer info */}
        <div className="pt-4 border-t-2 border-neo-border border-dashed text-center">
          <span className="neo-label-sm text-neo-text-muted">
            Local Host Node: <span className="font-mono text-neo-text">127.0.0.1:8080</span>
          </span>
        </div>
      </div>
    </div>
  );
}
