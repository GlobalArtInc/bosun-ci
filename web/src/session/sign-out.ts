import { useMutation, useQueryClient } from '@tanstack/react-query';
import { useNavigate } from 'react-router';
import { useApi } from '../api/context';
import { forgetSession, ME, signInPath } from './session';

export function useSignOut() {
  const api = useApi();
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  return useMutation({
    mutationFn: () => api.signOut(),
    onSuccess: () => {
      forgetSession(queryClient);
      navigate(signInPath('/'), { replace: true });
    },
    onError: () => queryClient.invalidateQueries({ queryKey: ME }),
  });
}
